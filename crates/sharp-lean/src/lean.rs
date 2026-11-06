//! The Lean step of `mago compile`: proves each law from its `.sharp` file, and refuses each `.sharp` file whose law is
//! not proven.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_analyzer::graph::strongly_connected_parts;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::FileId;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_sharp_bridge::unit;

use crate::goal;
use crate::goal::Names;
use crate::issues;
use crate::library;
use crate::library::Stop;
use crate::package;
use crate::package::Package;
use crate::package::Program;
use crate::package::Toolchain;
use crate::package::place_of;
use crate::runner;
use crate::runner::Answer;
use crate::runner::Check;
use crate::runner::Judgment;
use crate::runner::Theorem;
use crate::translate::Law;
use crate::translate::Translation;

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

    /// Proves the laws of `translations`, which hold every accepted `.sharp` file, and returns the issues that refuse
    /// each file whose law is not proven.
    ///
    /// A file whose `.sharpc` is current is proved: the `.sharpc` lists every source its laws reach as inputs, and the
    /// build ID it records covers the runtime library. No Lean process starts when no file states a law, nor when the
    /// `.sharpc` of each file that states one is current. When no file states a law, `.sharp/.lean/` is deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be read or written, when the runtime library does not build, or when the
    /// runner fails.
    pub fn check(&self, translations: Vec<Translation>) -> io::Result<HashMap<FileId, IssueCollection>> {
        let mut package = Package::new(&self.root);
        let mut issues: HashMap<FileId, IssueCollection> = HashMap::default();

        if translations.iter().all(|translation| translation.laws.is_empty()) {
            match fs::remove_dir_all(&package.directory) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(DatabaseError::io(IOAction::DeleteFolder, &package.directory, error).into());
                }
                _ => return Ok(issues),
            }
        }

        let mut stale = Vec::new();
        for translation in translations.iter().filter(|translation| !translation.laws.is_empty()) {
            if self.is_stale(translation)? {
                stale.push(translation);
            }
        }
        if stale.is_empty() {
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
                // Every accepted file is translated, so a law a body calls that no translation states is in a file
                // with errors.
                let refused: Vec<&str> = law
                    .proof
                    .laws
                    .iter()
                    .map(|(called, _)| called.as_str())
                    .filter(|called| !laws.position.contains_key(called))
                    .collect();
                for called in &refused {
                    report(&mut issues, translation, issues::refused_reach(law, &place_of(called)));
                }
                if laws.cyclic.contains(law.statement.as_str()) {
                    for (called, span) in
                        law.proof.laws.iter().filter(|(called, _)| laws.cyclic.contains(called.as_str()))
                    {
                        report(&mut issues, translation, issues::law_cycle(*span, called));
                    }
                }
                sound &= problems.is_empty() && refused.is_empty() && !laws.cyclic.contains(law.statement.as_str());
            }
            if sound {
                work.push(translation);
            }
        }
        if work.is_empty() {
            return Ok(issues);
        }

        let toolchain = package.toolchain()?;
        if !matches!(toolchain, Toolchain::Ready) {
            if let Toolchain::Failed(failure) = &toolchain {
                tracing::debug!("The law checker could not be installed: {failure}");
            }
            for (translation, span) in
                work.iter().filter_map(|translation| Some((*translation, translation.laws.first()?.span)))
            {
                let issue = match &toolchain {
                    Toolchain::Failed(_) => issues::uninstalled_checker(span),
                    _ => issues::uninstallable_lean(span),
                };
                report(&mut issues, translation, issue);
            }
            return Ok(issues);
        }

        let library = library::build(&package.lake)?;
        let modules = program.modules(&work.iter().map(|translation| translation.file).collect::<Vec<_>>());
        package.write(&library)?;
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

        Ok(issues)
    }

    /// Whether `translation`'s `.sharpc` is missing, or one of its inputs changed since it was written.
    fn is_stale(&self, translation: &Translation) -> io::Result<bool> {
        let source = translation.path.clone().unwrap_or_else(|| self.path(&translation.name));
        let Some(compiled) = unit::compiled_path(&self.root, &source)? else {
            return Ok(true);
        };

        match fs::read(&compiled) {
            Ok(bytes) => Ok(unit::is_stale(&bytes, |path| unit::stamp(&self.root, path))?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(DatabaseError::io(IOAction::ReadFile, &compiled, error).into()),
        }
    }

    /// The workspace-relative `name` as a path.
    fn path(&self, name: &[u8]) -> PathBuf {
        self.root.join(String::from_utf8_lossy(name).as_ref())
    }
}

/// What proving the laws of the stale files reads: the package the runner runs in, the runtime library, every
/// declaration and law of the workspace, the generated modules, and how PHP# calls each member a goal names.
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
    /// each law they call, transitively, before them. A law that calls a law that cannot be proven is not proven. A
    /// generated module Lean does not elaborate refuses each law of the check that reaches it, and the next runner
    /// checks the file's other laws without it.
    fn prove(&self, work: Vec<&Translation>, issues: &mut HashMap<FileId, IssueCollection>) -> io::Result<()> {
        let mut stopped: Vec<(String, Stop)> = Vec::new();
        let mut pending: Vec<(&Translation, Vec<&Law>)> =
            work.into_iter().map(|translation| (translation, translation.laws.iter().collect())).collect();
        while !pending.is_empty() {
            // One runner answers the check of every file, so Lean and the runtime library are imported once per compile.
            let (judgments, checks): (Vec<_>, Vec<_>) =
                pending.iter().map(|(translation, laws)| self.check(translation, laws)).unzip();
            let answers = runner::check(self.library, self.package, self.modules, &checks, &mut stopped)?;
            let mut left = Vec::new();
            for ((((translation, laws), check), mut judgments), answer) in
                pending.into_iter().zip(&checks).zip(judgments).zip(answers)
            {
                match answer {
                    Answer::Judged(answered) => {
                        let order =
                            check.theorems.iter().filter(|theorem| theorem.judged).map(|theorem| theorem.statement);
                        for (statement, judgment) in order.zip(answered) {
                            if let Some(law) = laws.iter().find(|law| law.statement == statement) {
                                judgments.push((law, judgment));
                            }
                        }
                        for (law, judgment) in judgments {
                            for issue in judged(law, judgment, &self.receivers, &self.declared) {
                                report(issues, translation, issue);
                            }
                        }
                    }
                    Answer::Refused(module, refusal) => {
                        let (mut refused, mut kept): (Vec<&Law>, Vec<&Law>) =
                            laws.iter().partition(|law| self.reaches(law, &module));
                        // The check reaches only modules its laws reach, so some law reaches `module`; each runner then
                        // gets fewer laws, and the loop ends.
                        if refused.is_empty() {
                            refused = std::mem::take(&mut kept);
                        }
                        for law in refused {
                            report(issues, translation, issues::generated_module_refused(law, &module, &refusal));
                        }
                        if !kept.is_empty() {
                            left.push((translation, kept));
                        }
                    }
                }
            }
            pending = left;
        }

        Ok(())
    }

    /// Whether `law` reaches the generated `module`: the module of the law, or of a law it calls, transitively, is that
    /// module or imports it, transitively.
    fn reaches(&self, law: &Law, module: &str) -> bool {
        let hosts =
            self.laws.closure(law).into_iter().map(|position| self.host_of(self.laws.ordered[position].0)).collect();

        imports_reach(hosts, self.modules, module)
    }

    /// How `laws`, of `translation`, are judged without the runner, each through a law it calls that cannot be proven,
    /// and the check the runner judges the others with. The check reaches the file's own generated module even when no
    /// theorem is left to elaborate, so a module Lean rejects refuses the file's laws.
    fn check<'law>(
        &'law self,
        translation: &'law Translation,
        laws: &[&'law Law],
    ) -> (Vec<(&'law Law, Judgment)>, Check<'law>) {
        let ordered = self.laws;
        let mut theorems: Vec<usize> = Vec::new();
        let mut judgments: Vec<(&Law, Judgment)> = Vec::new();
        for &law in laws {
            let closure = ordered.closure(law);
            let unproven = closure.iter().map(|position| ordered.ordered[*position].1).find(|called| {
                called.statement != law.statement
                    && (ordered.cyclic.contains(called.statement.as_str())
                        || !self.program.problems(&called.statement).is_empty())
            });
            match unproven {
                Some(called) => judgments.push((law, Judgment::Through(called.statement.clone()))),
                None => theorems.extend(closure),
            }
        }
        theorems.sort_unstable();
        theorems.dedup();

        let judged: Vec<&Law> = laws
            .iter()
            .copied()
            .filter(|law| !judgments.iter().any(|(blocked, _)| blocked.statement == law.statement))
            .collect();
        let mut modules: Vec<String> = theorems
            .iter()
            .map(|position| self.host_of(ordered.ordered[*position].0))
            .chain(std::iter::once(self.host_of(translation)))
            .collect();
        modules.sort();
        modules.dedup();
        modules.push(PROOF_STEPS.to_owned());
        let theorems: Vec<Theorem<'_>> = theorems
            .iter()
            .map(|position| {
                let law = ordered.ordered[*position].1;
                Theorem {
                    statement: &law.statement,
                    text: &law.proof.text,
                    judged: judged.iter().any(|judged| judged.statement == law.statement),
                }
            })
            .collect();

        (judgments, Check { modules, theorems })
    }
}

/// The issues of `law` the runner's `judgment` gives: one for each case and each call whose goal it left open, with
/// the goal in PHP#, which names the members of the code as `receivers` and `declared` hold them.
fn judged(law: &Law, judgment: Judgment, receivers: &HashMap<String, bool>, declared: &HashSet<String>) -> Vec<Issue> {
    match judgment {
        Judgment::Proved => Vec::new(),
        Judgment::Through(called) => vec![issues::through(law, &called)],
        Judgment::Rejected(error) => {
            tracing::debug!("The law checker rejected the proof of law {}: {error}", law.name);

            vec![issues::rejected_proof(law)]
        }
        // A case a `match` reaches both where a guard fails and where a pattern does is one case, reported once.
        Judgment::Open(steps) => steps
            .iter()
            .enumerate()
            .filter(|(place, step)| {
                !steps[..*place].iter().any(|before| before.index == step.index && before.condition == step.condition)
            })
            .map(|(_, step)| {
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

/// Whether one of the generated modules `from` is `module`, or imports it, transitively, through the imports `modules`
/// hold.
fn imports_reach(mut from: Vec<String>, modules: &[package::Module], module: &str) -> bool {
    let mut seen: HashSet<String> = HashSet::default();
    while let Some(next) = from.pop() {
        if next == module {
            return true;
        }
        if let Some(found) = modules.iter().find(|candidate| candidate.name == next) {
            from.extend(found.imports.iter().filter(|imported| !seen.contains(*imported)).cloned());
        }
        seen.insert(next);
    }

    false
}

#[cfg(test)]
mod tests {
    use mago_span::Span;

    use super::*;
    use crate::package::Refusal;

    fn law() -> Law {
        Law {
            name: "addKeepsCurrency".to_owned(),
            statement: "App.Shared.Money.addKeepsCurrency".to_owned(),
            span: Span::dummy(0, 0),
            proof: crate::translate::proof::Proof::default(),
        }
    }

    /// A proof or a module the checker wrote wrong is the checker's bug, which the message says in PHP#, naming no Lean
    /// module and quoting no Lean error.
    #[test]
    fn a_proof_or_a_module_the_checker_wrote_wrong_names_no_lean() {
        let rejected = judged(
            &law(),
            Judgment::Rejected("unknown identifier 'Code.App.Shared.Money.add'".to_owned()),
            &HashMap::default(),
            &HashSet::default(),
        );
        let refused = issues::generated_module_refused(&law(), "Code.App.Shared.Money", &Refusal::Rejected(26));

        assert_eq!(
            rejected.iter().map(|issue| issue.message.as_str()).collect::<Vec<_>>(),
            [
                "Law addKeepsCurrency is not proven: the checker wrote a proof of it that does not check. This is a bug in the PHP# checker; report it to its maintainers."
            ]
        );
        assert_eq!(
            refused.message,
            "Law addKeepsCurrency is not proven: the checker translated the code it reaches wrongly. This is a bug in the PHP# checker; report it to its maintainers."
        );
    }

    /// A refused module refuses a law whose modules are it or import it, through any chain of imports, and no law whose
    /// modules do not.
    #[test]
    fn a_law_reaches_a_module_its_own_modules_are_or_import_transitively() {
        let module = |name: &str, imports: &[&str]| package::Module {
            name: name.to_owned(),
            host: name.to_owned(),
            imports: imports.iter().map(|imported| (*imported).to_owned()).collect(),
            text: String::new(),
        };
        let modules = [
            module("Code.App.Money", &[]),
            module("Code.App.Wallet", &["Code.App.Money"]),
            module("Code.App.Bank", &["Code.App.Wallet"]),
            module("Code.App.Clock", &[]),
        ];
        let reaches = |from: &[&str]| {
            imports_reach(from.iter().map(|host| (*host).to_owned()).collect(), &modules, "Code.App.Money")
        };

        assert!(reaches(&["Code.App.Money"]));
        assert!(reaches(&["Code.App.Clock", "Code.App.Bank"]));
        assert!(!reaches(&["Code.App.Clock"]));
        assert!(!reaches(&[]));
    }
}
