//! The Lean step of `mago compile`: proves each law from its `.sharp` file, checks each structure rule, and refuses each
//! `.sharp` file whose law is not proven or that breaks a rule.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_analyzer::graph::strongly_connected_parts;
use mago_database::file::FileId;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_sharp_bridge::unit;
use rayon::prelude::*;

use crate::goal;
use crate::goal::Names;
use crate::issues;
use crate::library;
use crate::package;
use crate::package::Package;
use crate::package::Program;
use crate::package::Root;
use crate::package::Toolchain;
use crate::rules;
use crate::rules::Unbuilt;
use crate::runner;
use crate::runner::Judgment;
use crate::runner::Theorem;
use crate::translate::Law;
use crate::translate::Translation;

/// The folder Composer installs packages into.
pub(crate) const VENDOR: &[u8] = b"vendor/";

/// The module of the steps every law's proof is written in.
const PROOF_STEPS: &str = "Sharp.Lean.Proof";

/// The Lean step of one workspace.
#[derive(Debug)]
pub struct Lean {
    root: PathBuf,
}

/// Every law of the workspace, in an order where each comes after each law it calls.
struct Laws<'laws> {
    ordered: Vec<(&'laws Translation, &'laws Law)>,
    /// The position of each law in `ordered`, by its statement.
    position: HashMap<&'laws str, usize>,
    /// The laws that call each other in a cycle.
    cyclic: HashSet<&'laws str>,
}

impl<'laws> Laws<'laws> {
    fn new(translations: &'laws [Translation]) -> Self {
        let all: Vec<(&Translation, &Law)> = translations
            .iter()
            .flat_map(|translation| translation.laws.iter().map(move |law| (translation, law)))
            .collect();
        let index: HashMap<&str, usize> =
            all.iter().enumerate().map(|(index, (_, law))| (law.statement.as_str(), index)).collect();
        let successors: Vec<Vec<usize>> = all
            .iter()
            .map(|(_, law)| {
                law.proof.laws.iter().filter_map(|(called, _)| index.get(called.as_str()).copied()).collect()
            })
            .collect();

        let mut ordered = Vec::with_capacity(all.len());
        let mut cyclic = HashSet::default();
        for part in strongly_connected_parts(&successors) {
            if part.len() > 1 {
                cyclic.extend(part.iter().map(|node| all[*node].1.statement.as_str()));
            }
            ordered.extend(part.into_iter().map(|node| all[node]));
        }
        let position =
            ordered.iter().enumerate().map(|(position, (_, law))| (law.statement.as_str(), position)).collect();

        Self { ordered, position, cyclic }
    }

    /// `law` and each law it calls, transitively, in order.
    fn closure(&self, law: &Law) -> Vec<usize> {
        let mut reached = Vec::new();
        let mut pending = vec![law.statement.as_str()];
        while let Some(next) = pending.pop() {
            let Some(&position) = self.position.get(next) else {
                continue;
            };
            if reached.contains(&position) {
                continue;
            }
            reached.push(position);
            pending.extend(self.ordered[position].1.proof.laws.iter().map(|(called, _)| called.as_str()));
        }
        reached.sort_unstable();

        reached
    }
}

impl Lean {
    /// The Lean step of the workspace at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Lean {
        Lean { root: root.to_path_buf() }
    }

    /// Proves the laws of `translations`, which hold every accepted `.sharp` file, checks the structure rules of every
    /// `Module.lean` over their facts, and returns the issues that refuse each file whose law is not proved or that
    /// breaks a rule. `namespaces` holds the namespace of every class-like the codebase declares, accepted or not,
    /// which a rule names.
    ///
    /// A file whose `.sharpc` is current is proved: the `.sharpc` lists every source its laws reach as inputs, and the
    /// build ID it records covers the runtime library. Every rule runs over every file's facts whenever one `.sharpc`
    /// is not current, the namespaces changed, or a workspace Lean file the check builds changed, joined or left, so a
    /// compile checks every rule exactly. Every Lean file of a PSR-4 folder that no Lean file imports is built then too,
    /// so a `@[rule]` outside a `Module.lean` is an error, though no `.sharpc` lists that file. A `Module.lean` in no
    /// PSR-4 folder, and a Lean file in a PSR-4 folder under a root Lean or Mago owns, each refuse every file under
    /// their folder. No Lean process starts when no file states a law and no workspace Lean file is left to build, nor
    /// when each `.sharpc` is current and neither the namespaces nor those Lean files changed. When none of those
    /// remains, `.sharp/.lean/` is deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be read or written, when the runtime library does not build, or when the
    /// runner fails.
    pub fn check(
        &self,
        translations: Vec<Translation>,
        namespaces: &[String],
    ) -> io::Result<HashMap<FileId, IssueCollection>> {
        let mut package = Package::new(&self.root);
        let mut issues: HashMap<FileId, IssueCollection> = HashMap::default();

        let roots = self.roots(translations.iter())?;
        let (files, unbuilt) = rules::find(&self.root, &roots, &translations)?;
        for unbuilt in &unbuilt {
            let (Unbuilt::Unnamed(path) | Unbuilt::Owned(path, _)) = unbuilt;
            let folder = &path.as_bytes()[..path.rfind('/').map_or(0, |end| end + 1)];
            for translation in translations
                .iter()
                .filter(|translation| translation.name.starts_with(folder) && !translation.name.starts_with(VENDOR))
            {
                let anchor = translation.facts.anchor;
                let issue = match unbuilt {
                    Unbuilt::Unnamed(path) => issues::no_rules_module(anchor, path),
                    Unbuilt::Owned(path, namespace) => issues::owned_lean(anchor, namespace, path),
                };
                report(&mut issues, translation, issue);
            }
        }
        if translations.iter().all(|translation| translation.laws.is_empty()) && files.is_empty() {
            match fs::remove_dir_all(&package.directory) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => return Ok(issues),
            }
        }

        let mut stale = Vec::new();
        for translation in translations.iter().filter(|translation| !translation.laws.is_empty()) {
            if self.is_stale(translation)? {
                stale.push(translation);
            }
        }
        let declared = package::namespaces(namespaces);
        let sources = rules::sources(&self.root, &files)?;
        let check_rules = !files.is_empty()
            && (!stale.is_empty()
                || !package.holds(&declared)
                || !package.checked(&sources)
                || self.any_stale(&translations)?);
        if stale.is_empty() && !check_rules {
            return Ok(issues);
        }

        let program = Program::new(&translations);
        let laws = Laws::new(&translations);
        let translated: Vec<FileId> = translations.iter().map(|translation| translation.file).collect();
        let mut work = Vec::new();
        for translation in stale {
            let mut sound = true;
            for law in &translation.laws {
                let problems = program.problems(&law.statement);
                for construct in &problems.unmodeled {
                    report(&mut issues, translation, issues::unmodeled(law, construct));
                }
                for missing in &problems.missing {
                    let issue = if missing.file.is_some_and(|file| translated.contains(&file)) {
                        issues::untranslated(law, &missing.place)
                    } else {
                        issues::refused_reach(law, &missing.place)
                    };
                    report(&mut issues, translation, issue);
                }
                if laws.cyclic.contains(law.statement.as_str()) {
                    for (called, span) in
                        law.proof.laws.iter().filter(|(called, _)| laws.cyclic.contains(called.as_str()))
                    {
                        report(&mut issues, translation, issues::law_cycle(*span, called));
                    }
                }
                sound &= problems.is_empty() && !laws.cyclic.contains(law.statement.as_str());
            }
            if sound {
                work.push(translation);
            }
        }
        if work.is_empty() && !check_rules {
            return Ok(issues);
        }

        let toolchain = package.toolchain()?;
        if !matches!(toolchain, Toolchain::Ready) {
            let mut waiting: Vec<(&Translation, mago_span::Span)> =
                work.iter().filter_map(|translation| Some((*translation, translation.laws.first()?.span))).collect();
            for translation in &translations {
                let judged = check_rules && files.iter().any(|file| rules::judges(file, translation));
                if judged && !waiting.iter().any(|(waiting, _)| waiting.file == translation.file) {
                    waiting.push((translation, translation.facts.anchor));
                }
            }
            for (translation, span) in waiting {
                let issue = match &toolchain {
                    Toolchain::Failed(message) => issues::elan_failed(span, message),
                    Toolchain::Missing(failure) => issues::lake_not_found(span, failure),
                    _ => issues::uninstallable_lean(span),
                };
                report(&mut issues, translation, issue);
            }
            return Ok(issues);
        }

        let library = library::build(&package.lake)?;
        let mut modules = program.modules(
            &work.iter().map(|translation| translation.file).collect::<Vec<_>>(),
            &files.iter().map(|file| file.module.as_str()).collect::<Vec<_>>(),
        );
        modules.push(declared);
        package.write(&library, &roots, &modules)?;
        if !work.is_empty() {
            let receivers: HashMap<String, bool> =
                translations.iter().flat_map(|translation| translation.receivers.iter().cloned()).collect();
            let declared: HashSet<String> = translations
                .iter()
                .flat_map(|translation| &translation.declarations)
                .map(|declaration| declaration.key.clone())
                .collect();
            Proofs {
                package: &package,
                library: &library,
                program: &program,
                laws: &laws,
                modules: &modules,
                receivers,
                declared,
            }
            .prove(work, &mut issues)?;
        }
        if check_rules {
            rules::check(&self.root, &roots, &library, &package, &files, &translations, &mut issues)?;
            package.record_checked(&sources)?;
        }

        Ok(issues)
    }

    /// Whether the `.sharpc` of one of `translations` is not current.
    fn any_stale(&self, translations: &[Translation]) -> io::Result<bool> {
        for translation in translations {
            if self.is_stale(translation)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Whether `translation`'s `.sharpc` is missing, or one of its inputs changed since it was written.
    fn is_stale(&self, translation: &Translation) -> io::Result<bool> {
        let source = translation.path.clone().unwrap_or_else(|| self.path(&translation.name));
        let Some(compiled) = unit::compiled_path(&self.root, &source)? else {
            return Ok(true);
        };

        match fs::read(compiled) {
            Ok(bytes) => unit::is_stale(&self.root, &bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// The PSR-4 folders of the workspace's `composer.json` and of each package in `vendor/` one of `translations`
    /// belongs to.
    fn roots<'translation>(
        &self,
        translations: impl Iterator<Item = &'translation Translation>,
    ) -> io::Result<Vec<Root>> {
        let mut roots = package::roots(&self.root)?;
        let mut packages: Vec<PathBuf> = Vec::new();
        for translation in translations {
            let Some(rest) = translation.name.strip_prefix(VENDOR) else {
                continue;
            };
            let mut parts = rest.split(|byte| *byte == b'/');
            if let (Some(vendor), Some(name), Some(_)) = (parts.next(), parts.next(), parts.next()) {
                let package = self
                    .path(VENDOR)
                    .join(String::from_utf8_lossy(vendor).as_ref())
                    .join(String::from_utf8_lossy(name).as_ref());
                if !packages.contains(&package) {
                    packages.push(package);
                }
            }
        }
        for package in packages {
            roots.extend(package::roots(&package)?);
        }

        Ok(roots)
    }

    /// The workspace-relative `name` as a path.
    fn path(&self, name: &[u8]) -> PathBuf {
        self.root.join(String::from_utf8_lossy(name).as_ref())
    }
}

/// What proving the laws of the stale files reads: the package Lake builds, the runtime library, every declaration and
/// law of the workspace, the generated modules, and how PHP# calls each member a goal names.
struct Proofs<'proofs> {
    package: &'proofs Package,
    library: &'proofs Path,
    program: &'proofs Program<'proofs>,
    laws: &'proofs Laws<'proofs>,
    modules: &'proofs [package::Module],
    receivers: HashMap<String, bool>,
    /// The Lean name of every declaration of the workspace.
    declared: HashSet<String>,
}

impl Proofs<'_> {
    /// The module of `translation`'s group that holds its declarations.
    fn host_of(&self, translation: &Translation) -> String {
        let name = package::module_name(translation);

        self.modules.iter().find(|module| module.name == name).map_or(name, |module| module.host.clone())
    }

    /// Proves the laws of `work` and reports each law not proven. Each file's laws are elaborated with the theorem of
    /// each law they call, transitively, before them. A law that calls a law that cannot be proven is not proven.
    fn prove(&self, mut work: Vec<&Translation>, issues: &mut HashMap<FileId, IssueCollection>) -> io::Result<()> {
        let called = work
            .iter()
            .flat_map(|translation| &translation.laws)
            .flat_map(|law| self.laws.closure(law))
            .map(|position| self.laws.ordered[position].0);
        let mut hosts: Vec<String> =
            work.iter().copied().chain(called).map(|translation| self.host_of(translation)).collect();
        hosts.sort();
        hosts.dedup();
        let failed = self.package.build(&hosts)?;
        for host in &failed.modules {
            let errors = self.package.errors(host, &failed)?;
            let line = errors.first().map_or(0, |error| error.pos.line);
            for translation in work.iter().filter(|translation| self.host_of(translation) == *host) {
                for law in &translation.laws {
                    report(issues, translation, issues::generated_module_refused(law, host, line));
                }
            }
        }
        work.retain(|translation| !failed.modules.contains(&self.host_of(translation)));

        // One runner per file, run on Mago's thread pool, so `threads` caps how many run at once.
        let answers: Vec<io::Result<Vec<(&Law, Judgment)>>> =
            work.par_iter().map(|translation| self.judge(translation)).collect();
        for (translation, answers) in work.into_iter().zip(answers) {
            for (law, judgment) in answers? {
                for issue in judged(law, judgment, &self.receivers, &self.declared) {
                    report(issues, translation, issue);
                }
            }
        }

        Ok(())
    }

    /// How the runner judges each law of `translation`.
    fn judge<'law>(&self, translation: &'law Translation) -> io::Result<Vec<(&'law Law, Judgment)>> {
        let laws = self.laws;
        let mut theorems: Vec<usize> = Vec::new();
        let mut judgments: Vec<(&Law, Judgment)> = Vec::new();
        for law in &translation.laws {
            let closure = laws.closure(law);
            let unproven = closure.iter().map(|position| laws.ordered[*position].1).find(|called| {
                called.statement != law.statement
                    && (laws.cyclic.contains(called.statement.as_str())
                        || !self.program.problems(&called.statement).is_empty())
            });
            match unproven {
                Some(called) => judgments.push((law, Judgment::Through(called.statement.clone()))),
                None => theorems.extend(closure),
            }
        }
        theorems.sort_unstable();
        theorems.dedup();
        if theorems.is_empty() {
            return Ok(judgments);
        }

        let judged: Vec<&Law> = translation
            .laws
            .iter()
            .filter(|law| !judgments.iter().any(|(blocked, _)| blocked.statement == law.statement))
            .collect();
        let request: Vec<Theorem<'_>> = theorems
            .iter()
            .map(|position| {
                let law = laws.ordered[*position].1;
                Theorem {
                    statement: &law.statement,
                    text: &law.proof.text,
                    judged: judged.iter().any(|judged| judged.statement == law.statement),
                }
            })
            .collect();
        let mut imports: Vec<String> =
            theorems.iter().map(|position| self.host_of(laws.ordered[*position].0)).collect();
        imports.sort();
        imports.dedup();
        imports.push(PROOF_STEPS.to_owned());
        let imports: Vec<&str> = imports.iter().map(String::as_str).collect();

        let answered = runner::check(self.library, self.package, &imports, &request)?;
        let order = request.iter().filter(|theorem| theorem.judged).map(|theorem| theorem.statement);
        for (statement, judgment) in order.zip(answered) {
            if let Some(law) = judged.iter().find(|law| law.statement == statement) {
                judgments.push((law, judgment));
            }
        }

        Ok(judgments)
    }
}

/// The issues of `law` the runner's `judgment` gives: one for each case and each call whose goal it left open, with
/// the goal in PHP#, which names the members of the code as `receivers` and `declared` hold them.
fn judged(law: &Law, judgment: Judgment, receivers: &HashMap<String, bool>, declared: &HashSet<String>) -> Vec<Issue> {
    match judgment {
        Judgment::Proved => Vec::new(),
        Judgment::Through(called) => vec![issues::through(law, &called)],
        Judgment::Rejected(_) => vec![issues::rejected_proof(law)],
        Judgment::Open(steps) => steps
            .iter()
            .map(|step| {
                if step.condition {
                    let call = &law.proof.calls[step.index - 1];
                    let names = Names { receivers, declared, arguments: &call.arguments };
                    issues::unshown_condition(law, call, goal::conjunction(&step.goal, &names).as_deref())
                } else {
                    let case = &law.proof.cases[step.index - 1];
                    let names = Names { receivers, declared, arguments: &[] };
                    issues::unproven_case(law, case, goal::php(&step.goal, &names).as_deref())
                }
            })
            .collect(),
    }
}

fn report(issues: &mut HashMap<FileId, IssueCollection>, translation: &Translation, issue: Issue) {
    issues.entry(translation.file).or_default().push(issue);
}
