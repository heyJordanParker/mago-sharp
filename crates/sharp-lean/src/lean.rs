//! The Lean step of `mago compile`: proves each law from its `.sharp` file, and refuses each `.sharp` file whose law is
//! not proven.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::graph::strongly_connected_parts;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::FileId;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::goal;
use crate::goal::Names;
use crate::issues;
use crate::library;
use crate::library::Stop;
use crate::library::Toolchain;
use crate::package;
use crate::package::Package;
use crate::package::Program;
use crate::package::Refusal;
use crate::package::place_of;
use crate::package::write_if_changed;
use crate::reach::Reach;
use crate::runner;
use crate::runner::Answer;
use crate::runner::Check;
use crate::runner::Judgment;
use crate::runner::Theorem;
use crate::translate::Law;
use crate::translate::Translation;
use crate::translate::names;

/// The steps the theorems a runner elaborates close their cases with.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// Lean's search, which `Sharp/Lean/Proof.lean` writes, as the text of each law's theorem holds it.
    Search,
    /// The fixed steps of [`steps`], for laws without a body, which need only the runtime library.
    Steps,
    /// The fixed steps where a law has them, and Lean's search for each law without them and each law they leave
    /// unproven, in the same runner, which imports `Sharp/Lean/Proof.lean`: the fixed steps of a law with a body use
    /// its `sharp_call%` and its rewrites.
    StepsThenSearch,
}

impl Form {
    /// The steps a runner of this form proves with, as the laws phase logs it.
    fn steps(self) -> &'static str {
        match self {
            Form::Search => "Lean's search",
            Form::Steps => "the fixed steps",
            Form::StepsThenSearch => "the fixed steps, then Lean's search",
        }
    }
}

/// The module of the steps a law's theorem in the [`Form::Search`] form is written in.
const PROOF_STEPS: &str = "Sharp.Lean.Proof";

/// The module of the runtime library that states a law, all a theorem in the [`Form::Steps`] form needs.
const LAW_STATEMENT: &str = "Sharp.Lean.Law";

/// The rewrites of the fixed steps' first two passes, beside the definitions the law uses: `sharp_step`, the simp set of
/// `Sharp/Lean/Steps.lean` whose lemmas `Sharp/Lean/Law.lean` tags, so the library builds its index once, and the
/// simprocs that decide an `if`'s condition and a case no value reaches. The set holds the monad's steps, a law's claim
/// and each choice it makes, a fact about the values a step binds, which a law's `when` and a body's `sharp_call%` state
/// and `Sharp/Lean/Proof.lean`'s search reads one by one too, a goal over a value one of several branches gives, which
/// holds when it holds for each, the facts a case holds, split one by one, the comparisons, `b ≤ a` as `¬ a < b` on an
/// `int` and on the exact `Int` a condition computes, so a fact that one comparison states decides a test that the other
/// writes, and a value a nullable side holds or lacks. An int step stays folded, so two steps that compute from the same
/// values stay one term, which `simp_all` reads as one value, as a law that runs a method twice needs, as `at.plus(d)` in
/// a claim and in a method it calls.
const MERGING_REWRITES: &[&str] = &["sharp_step", "↓reduceIte", "reduceCtorEq"];

/// The rewrites of the fixed steps' third pass and of their last rewrite of a goal the passes leave open: `sharp_full`,
/// the lemmas of `sharp_step` and the ones that read an int step as the `Int` it gives, and the simprocs of
/// [`MERGING_REWRITES`] and of an int literal's value.
const FULL_REWRITES: &[&str] = &["sharp_full", "↓reduceIte", "reduceCtorEq", "Int64.reduceToInt"];

/// The lemmas that read a law's `when` as one fact for each step it computes, in each case of each branch it takes,
/// as `holds_bind` reads the law's claim, so the claim's own steps are those facts.
const CONDITION_LEMMAS: &[&str] = &[
    "bind_eq_pure",
    "bind_assoc",
    "pure_bind",
    "ite_eq_pure",
    "or_imp",
    "forall_and",
    "forall_exists_index",
    "and_imp",
    "pure_inj",
    "decide_eq_true_eq",
    "← Int64.not_lt",
    "← Int.not_lt",
];

/// How the Lean name of each declaration of the standard library starts. The first pass of the fixed steps leaves them
/// folded, so a value a law's `when` and its claim both compute with them is one value.
const LIBRARY: &str = "«Sharp».";

/// The record in the `.sharp` folder of how each law's proof ended, by the proof's key.
pub const PROOFS: &str = "proofs.json";

/// The heartbeats, in thousands, each law's theorem may use, as Lean counts the work it does: Lean's own default for a
/// command, `maxHeartbeats`. On the M2 Max it was measured on, 1,000 of them took 0.72 processor seconds, so a theorem
/// that never ends stops after about 140, near the 120 each file's check had before. Each of the 10 heavier laws of a
/// 20-law bench ran out of time at 35,000 and proved at 50,000, and each of the other 10 proved at 2,000.
const HEARTBEATS: u32 = 200_000;

/// What [`Lean::check`] found.
#[derive(Debug)]
pub struct Checked {
    /// The issues that refuse each file whose law is not proven.
    pub issues: HashMap<FileId, IssueCollection>,
    /// Whether [`PROOFS`] keeps the outcome of each law the check gave one, so a compile with every input as it was gets
    /// each of them again. It doesn't keep a refusal by a generated module, nor that no law checker could be installed,
    /// which depend on what no record holds.
    pub outcomes_kept: bool,
}

/// The Lean step of one workspace.
#[derive(Debug)]
pub struct Lean {
    root: PathBuf,
    threads: usize,
    /// The variables of the environment the run reads, which name Mago's cache folder and elan's, where the step finds
    /// Lean and builds its library.
    environment: BTreeMap<OsString, OsString>,
}

/// Every law of the translations, in an order where each comes after each law it calls.
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
                law.proof.laws.iter().filter_map(|(called, ..)| index.get(called.as_str()).copied()).collect()
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
        let mut reached = HashSet::default();
        let mut pending = vec![law.statement.as_str()];
        while let Some(next) = pending.pop() {
            let Some(&position) = self.position.get(next) else {
                continue;
            };
            if !reached.insert(position) {
                continue;
            }
            pending.extend(self.ordered[position].1.proof.laws.iter().map(|(called, ..)| called.as_str()));
        }
        let mut reached: Vec<usize> = reached.into_iter().collect();
        reached.sort_unstable();

        reached
    }
}

/// How a law's proof ended this compile: as Lean judged it, which the record keeps, or refused by the generated module
/// of this name, which the record never keeps. A module Lean rejected can be fixed by an edit the law's key does not
/// hold, as one of another law's names, and a module the checker's limits stopped depends on the machine's load.
type Ended = Result<Judgment, (String, Refusal)>;

/// [`PROOFS`]: how Lean judged each law, by the proof's key, as [`proof_key`] gives it. A law whose key is in it is never
/// proved again, whichever build of Mago wrote it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Record {
    proofs: BTreeMap<String, Recorded>,
}

/// One law's judgment in [`Record`].
#[derive(Debug, Serialize, Deserialize)]
struct Recorded {
    /// The workspace-relative name of the file that states the law.
    file: String,
    statement: String,
    judgment: Judgment,
}

impl Record {
    /// The record at `path`, or an empty one when none is there.
    fn read(path: &Path) -> Self {
        fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok()).unwrap_or_default()
    }

    /// Keeps each proof of a law that a file of `law_files` states and whose key `keys` doesn't replace, as it holds
    /// each law of this compile's translations by its statement.
    fn keep(&mut self, law_files: &HashSet<FileId>, keys: &HashMap<&str, String>) {
        self.proofs.retain(|key, recorded| {
            law_files.contains(&FileId::new(recorded.file.as_bytes()))
                && keys.get(recorded.statement.as_str()).is_none_or(|current| current == key)
        });
    }

    /// Writes the record to `path`, unless it holds the same bytes.
    fn write(self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(&self).map_err(io::Error::other)?;
        write_if_changed(path, &bytes)?;

        Ok(())
    }
}

impl Lean {
    /// The Lean step of the workspace at `root`, which proves laws on `threads` threads, with Lean and its library in
    /// the folders `environment` names.
    #[must_use]
    pub fn new(root: &Path, threads: usize, environment: &BTreeMap<OsString, OsString>) -> Lean {
        Lean { root: root.to_path_buf(), threads, environment: environment.clone() }
    }

    /// Proves the laws of `translations` and returns the issues that refuse each file whose law is not proven, and
    /// whether [`PROOFS`] keeps each outcome, as [`Checked`] holds them. `translations` hold each file whose laws a compile checks, and each file those laws reach, as `reach` gives
    /// them, but the files whose errors refuse them. `errored` holds each file the analysis reported an error in: a law
    /// that reaches a declaration no translation holds is refused for the errors of its file when that file is in it,
    /// and else as the checker's bug, as a lowering that panicked leaves its file untranslated.
    ///
    /// A law is judged once per key, [`proof_key`]: [`PROOFS`] keeps how Lean judged it, proved or not, and a law whose
    /// key it holds gets that judgment again without Lean. A law refused by a generated module is checked again by the
    /// next compile. The rest are proved by one runner on the compile's threads,
    /// sent only the generated modules its checks reach: each law without a body with fixed steps, and each law with a
    /// body, and each one those steps leave unproven, with Lean's search. No Lean process starts when every law's key
    /// is kept. When no file states a law, `.sharp/.lean/` and [`PROOFS`] are deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be read or written, when the runtime library does not build, or when a
    /// runner fails.
    pub fn check(
        &self,
        translations: Vec<Translation>,
        reach: &Reach,
        errored: &HashSet<FileId>,
    ) -> io::Result<Checked> {
        let package = Package::new(&self.root);
        let record_path = package.directory.with_file_name(PROOFS);
        let mut issues: HashMap<FileId, IssueCollection> = HashMap::default();

        let law_files: HashSet<FileId> = reach.law_files().collect();
        if law_files.is_empty() {
            match fs::remove_dir_all(&package.directory) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(DatabaseError::io(IOAction::DeleteFolder, &package.directory, error).into());
                }
                _ => {}
            }
            return match fs::remove_file(&record_path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    Err(DatabaseError::io(IOAction::DeleteFile, &record_path, error).into())
                }
                _ => Ok(Checked { issues, outcomes_kept: true }),
            };
        }

        let keying = Instant::now();
        let mut record = Record::read(&record_path);
        let program = Program::new(&translations);
        let laws = Laws::new(&translations);
        let scripts: HashMap<&str, String> = laws
            .ordered
            .iter()
            .filter_map(|(_, law)| Some((law.statement.as_str(), scripted(law, &program)?)))
            .collect();
        let settings = settings(&library::sources(), HEARTBEATS);
        let receivers: HashMap<String, bool> = translations
            .iter()
            .flat_map(|translation| &translation.receivers)
            .map(|(member, property)| (names::unescaped(member), *property))
            .collect();
        let declared: HashSet<String> = translations
            .iter()
            .flat_map(|translation| &translation.declarations)
            .map(|declaration| names::unescaped(&declaration.key))
            .collect();
        let sealed: HashSet<String> =
            reach.sealed().map(|interface| names::unescaped(&names::full_name(interface.as_bytes()))).collect();
        let names = Names { receivers: &receivers, declared: &declared, sealed: &sealed, arguments: &[] };
        let mut keys: HashMap<&str, String> = HashMap::default();
        let mut work: Vec<(&Translation, Vec<&Law>)> = Vec::new();
        for translation in translations.iter().filter(|translation| !translation.laws.is_empty()) {
            let mut proving = Vec::new();
            for law in &translation.laws {
                let refusals = refused(law, &program, &laws, errored);
                if !refusals.is_empty() {
                    for issue in refusals {
                        report(&mut issues, translation, issue);
                    }
                    continue;
                }

                let key = proof_key(law, &program, &laws, &scripts, &settings);
                match record.proofs.get(&key) {
                    Some(recorded) => {
                        for issue in judged(law, recorded.judgment.clone(), &names) {
                            report(&mut issues, translation, issue);
                        }
                    }
                    None => proving.push(law),
                }
                keys.insert(law.statement.as_str(), key);
            }
            if !proving.is_empty() {
                work.push((translation, proving));
            }
        }
        record.keep(&law_files, &keys);
        tracing::debug!("Found the proof keys of {} laws in {:.2} s.", keys.len(), keying.elapsed().as_secs_f64());
        tracing::debug!(
            "{} laws keep the outcome of their proof, and {} laws are proved.",
            keys.len() - work.iter().map(|(_, laws)| laws.len()).sum::<usize>(),
            work.iter().map(|(_, laws)| laws.len()).sum::<usize>(),
        );
        if work.is_empty() {
            record.write(&record_path)?;

            return Ok(Checked { issues, outcomes_kept: true });
        }

        let writing = Instant::now();
        package.write_toolchain()?;
        let mut wrote = writing.elapsed();
        let finding = Instant::now();
        let library = match library::built(&self.environment) {
            Some(library) => library,
            None => match library::toolchain(&self.environment) {
                Toolchain::Ready(lake) => library::build(&self.environment, &lake)?,
                toolchain => {
                    for (translation, span) in
                        work.iter().filter_map(|(translation, _)| Some((*translation, translation.laws.first()?.span)))
                    {
                        let issue = match &toolchain {
                            Toolchain::Failed(failure) => issues::uninstalled_checker(span, failure),
                            _ => issues::uninstallable_lean(span),
                        };
                        report(&mut issues, translation, issue);
                    }
                    record.write(&record_path)?;

                    return Ok(Checked { issues, outcomes_kept: false });
                }
            },
        };
        tracing::debug!("The law checker's library took {:.2} s to find or build.", finding.elapsed().as_secs_f64());
        let writing = Instant::now();
        package.remove_stale()?;
        let proved: HashSet<FileId> = work.iter().map(|(translation, _)| translation.file).collect();
        let proving = || work.iter().flat_map(|(_, proving)| proving);
        let reached: HashSet<&str> = proving()
            .flat_map(|law| laws.closure(law))
            .flat_map(|position| program.reached(&laws.ordered[position].1.statement))
            .chain(searched(proving().copied()).flat_map(|used| program.reached(used)))
            .collect();
        let modules = program.modules(&proved, &reached);
        wrote += writing.elapsed();
        tracing::debug!(
            "Wrote the Lean package of {} generated modules in {:.2} s.",
            modules.len(),
            wrote.as_secs_f64()
        );
        let proofs = Proofs {
            library: &library,
            threads: self.threads,
            program: &program,
            files: translations.iter().map(|translation| (translation.file, translation)).collect(),
            laws: &laws,
            modules: modules.iter().map(|module| (module.name.as_str(), module)).collect(),
            scripts: &scripts,
        };
        let mut outcomes_kept = true;
        for (translation, law, ended) in proofs.prove(work)? {
            let judgment = match ended {
                Ok(judgment) => judgment,
                Err((module, refusal)) => {
                    report(&mut issues, translation, issues::generated_module_refused(law, &module, &refusal));
                    outcomes_kept = false;
                    continue;
                }
            };
            for issue in judged(law, judgment.clone(), &names) {
                report(&mut issues, translation, issue);
            }
            if let Some(key) = keys.get(law.statement.as_str()) {
                record.proofs.insert(
                    key.clone(),
                    Recorded {
                        file: String::from_utf8_lossy(&translation.name).into_owned(),
                        statement: law.statement.clone(),
                        judgment,
                    },
                );
            }
        }
        record.write(&record_path)?;

        Ok(Checked { issues, outcomes_kept })
    }
}

/// What a law's proof runs on beside the texts its key hashes: the library's `sources`, which pin its Lean version, and
/// the `heartbeats` each theorem may use.
fn settings(sources: &str, heartbeats: u32) -> String {
    format!("{sources}\0{heartbeats}")
}

/// The Lean name of each declaration the search of each of `laws` for the small values that break it uses.
fn searched<'law>(laws: impl IntoIterator<Item = &'law Law>) -> impl Iterator<Item = &'law str> {
    laws.into_iter().flat_map(|law| &law.breaking).flat_map(|breaking| &breaking.uses).map(|used| used.key.as_str())
}

/// The key a law's proof ends once for: the hash of the `settings`, and the statement, theorem and fixed steps of the
/// law and of each law it calls, transitively, as `scripts` holds them, the text of the law's search for the small
/// values that break it, and the text of every declaration their statements and that search reach, as
/// [`Program::reached`] gives them, each in the order of its name. An edit of a declaration a law reaches changes it,
/// and so does a release that changes the library, the heartbeats or the fixed steps, and nothing else does.
fn proof_key(
    law: &Law,
    program: &Program<'_>,
    laws: &Laws<'_>,
    scripts: &HashMap<&str, String>,
    settings: &str,
) -> String {
    let mut hasher = Sha256::new();
    let mut part = |bytes: &[u8]| {
        hasher.update(format!("{}\0", bytes.len()));
        hasher.update(bytes);
    };
    part(settings.as_bytes());

    let called: BTreeMap<&str, &str> = laws
        .closure(law)
        .into_iter()
        .map(|position| laws.ordered[position].1)
        .map(|called| (called.statement.as_str(), called.proof.text.as_str()))
        .collect();
    let declarations: BTreeSet<&str> =
        called.keys().copied().chain(searched([law])).flat_map(|reaching| program.reached(reaching)).collect();
    part(law.breaking.as_ref().map_or("", |breaking| breaking.text.as_str()).as_bytes());
    for (statement, theorem) in called {
        part(statement.as_bytes());
        part(theorem.as_bytes());
        part(scripts.get(statement).map_or("", String::as_str).as_bytes());
    }
    for declaration in declarations {
        part(declaration.as_bytes());
        part(program.text(declaration).unwrap_or_default().as_bytes());
    }

    hasher.finalize().iter().fold(String::new(), |mut key, byte| {
        let _ = write!(key, "{byte:02x}");
        key
    })
}

/// The theorem of `law` with its fixed steps, which close it with the definitions its statement uses: a law without a
/// body closed by [`steps`], and a law with one with each case closed by them with the statement of each law whose fact
/// a case can hold too. An invariant of a machine has none.
/// A law without a body that states a `when`, which its one case holds as its first condition, first reads it with
/// [`CONDITION_LEMMAS`] and the program's own definitions it uses, then closes each case of each branch it takes.
fn scripted(law: &Law, program: &Program<'_>) -> Option<String> {
    let (plain, recursive) = program.definitions(&law.statement);
    if let Some(opening) = &law.proof.opening {
        let steps = steps(&[], &plain, &recursive);
        if law.proof.cases.first().is_none_or(|case| case.conditions.is_empty()) {
            return Some(format!("{opening}{steps}"));
        }
        let condition: Vec<&str> = CONDITION_LEMMAS.iter().copied().chain(own(&plain)).collect();

        return Some(format!(
            "{opening}revert «when»\n  simp only [{}]\n  and_intros\n  all_goals (intros; subst_vars)\n  all_goals ({steps})",
            condition.join(", ")
        ));
    }
    if law.proof.closes.is_empty() {
        return None;
    }
    let statements: Vec<&str> = std::iter::once(law.statement.as_str())
        .chain(law.proof.laws.iter().map(|(statement, ..)| statement.as_str()))
        .collect();

    let closed = format!("({})", steps(&statements, &plain, &recursive));

    Some(law.proof.closes.iter().fold(law.proof.text.clone(), |text, close| text.replace(close.as_str(), &closed)))
}

/// The fixed steps that close a case: `sharp_simp_all` with the `statements` of the laws a case holds and the definitions
/// `plain` as rewrites, first with [`MERGING_REWRITES`] and the program's own definitions alone, which rewrites the goal
/// with the facts that the standard library's calls give, then with [`MERGING_REWRITES`] and every definition, then with
/// [`FULL_REWRITES`], then `omega` or `decide` on each goal it leaves, or that rewrite again once each
/// value a fact names is in its place, as a case's value is where a fact names the constructor that built it. With
/// `recursive` definitions, the steps first unfold each of them once, as a recursion on a value the case does not know
/// needs, and else rewrite with them too, until each recursion ends, as one on values the law writes out needs.
fn steps(statements: &[&str], plain: &[&str], recursive: &[&str]) -> String {
    let closed = |definitions: &[&[&str]]| {
        let pass = |rewrites: &[&str], definitions: &[&str]| {
            let rewrites = [rewrites, statements, definitions].concat();
            format!("(try sharp_simp_all (config := {{ decide := true }}) only [{}])", rewrites.join(", "))
        };
        let reached: Vec<&str> = definitions.concat();
        let own: Vec<&str> = own(&reached).collect();
        let close = format!(
            "all_goals (intros; and_intros; all_goals (intros; subst_vars; first | rfl | omega | decide | ({}; done)))",
            pass(FULL_REWRITES, &reached)
        );

        format!(
            "({}; {}; {}; {close}; done)",
            pass(MERGING_REWRITES, &own),
            pass(MERGING_REWRITES, &reached),
            pass(FULL_REWRITES, &reached)
        )
    };
    let rewritten = closed(&[plain, recursive]);
    if recursive.is_empty() {
        return rewritten;
    }

    format!("first | (unfold {}; {}) | {rewritten}", recursive.join(" "), closed(&[plain]))
}

/// Each of `definitions` that the program declares, outside the standard library.
fn own<'definition>(definitions: &[&'definition str]) -> impl Iterator<Item = &'definition str> {
    definitions.iter().copied().filter(|definition| !definition.starts_with(LIBRARY))
}

/// `laws`, each with the translation that states it, one law to an entry, in order, so each is a check of its own,
/// which a runner's threads judge beside every other law's, its own file's too: one check of a file's laws judged them
/// one after another on one thread, so a compile whose slowest laws shared a file ran on one core.
fn by_law<'work>(
    laws: impl IntoIterator<Item = (&'work Translation, &'work Law)>,
) -> Vec<(&'work Translation, Vec<&'work Law>)> {
    laws.into_iter().map(|(translation, law)| (translation, vec![law])).collect()
}

/// What proving the laws of the stale files reads: the runtime library, the threads a runner proves them on, every
/// declaration and law of the translations, each translation by its file, the generated modules by name, and each
/// law's theorem in the [`Form::Steps`] form by its statement.
struct Proofs<'proofs> {
    library: &'proofs Path,
    threads: usize,
    program: &'proofs Program<'proofs>,
    files: HashMap<FileId, &'proofs Translation>,
    laws: &'proofs Laws<'proofs>,
    modules: HashMap<&'proofs str, &'proofs package::Module>,
    scripts: &'proofs HashMap<&'proofs str, String>,
}

/// One law whose proof ended, with the translation that states it.
type Proven<'work> = (&'work Translation, &'work Law, Ended);

impl Proofs<'_> {
    /// The module of `translation`'s group that holds its declarations.
    fn host_of(&self, translation: &Translation) -> String {
        let name = package::module_name(translation);

        self.modules.get(name.as_str()).map_or(name, |module| module.host.clone())
    }

    /// The Lean text of `law`'s search for the small values that break it, with the module of each declaration it
    /// uses, when Lean can elaborate it: it reads no construct Lean cannot model, and each declaration it uses is one a
    /// generated module of a file of the compile holds, which reads none either. None otherwise, and the runner runs no
    /// search for the law.
    fn search<'law>(&self, law: &'law Law) -> Option<(&'law str, Vec<String>)> {
        let breaking = law.breaking.as_ref().filter(|breaking| breaking.unmodeled.is_empty())?;
        let mut modules = Vec::new();
        for used in &breaking.uses {
            let translation = used.file.and_then(|file| self.files.get(&file))?;
            let module = self.modules.get(package::module_name(translation).as_str())?;
            let readable = used.names_only || self.program.problems(&used.key).is_empty();
            if self.program.text(&used.key).is_none() || !readable {
                return None;
            }
            modules.push(module.host.clone());
        }

        Some((breaking.text.as_str(), modules))
    }

    /// Proves the laws of `work`, and gives how each law's proof ended, in the order of `work`. When a law or one it
    /// calls has a body, one runner proves them all, each with its fixed steps, and with Lean's search when they leave
    /// it unproven or it has none, as an invariant of a machine. Otherwise the runner imports only
    /// the runtime library and proves them with the fixed steps, and each law they leave unproven, apart from one a
    /// generated module refuses or one that calls a law that cannot be proven, which ends the same either way, then
    /// proves with Lean's search in a runner after it, so its proof ends as that search ends it. It logs how long each
    /// law took in the runners and the heartbeats Lean counted for it, the most first.
    fn prove<'work>(&self, work: Vec<(&'work Translation, Vec<&'work Law>)>) -> io::Result<Vec<Proven<'work>>> {
        let order: HashMap<&str, usize> = work
            .iter()
            .flat_map(|(_, laws)| laws)
            .enumerate()
            .map(|(position, law)| (law.statement.as_str(), position))
            .collect();
        let searched = work
            .iter()
            .flat_map(|(_, laws)| laws)
            .flat_map(|law| self.laws.closure(law))
            .any(|position| self.laws.ordered[position].1.proof.opening.is_none());
        let work = by_law(work.iter().flat_map(|(translation, laws)| laws.iter().map(|law| (*translation, *law))));
        let mut stopped: Vec<(String, Stop)> = Vec::new();
        let mut spent: HashMap<&str, (Duration, u64)> = HashMap::default();
        let mut ended = Vec::new();
        if searched {
            ended.extend(self.run(work, Form::StepsThenSearch, &mut stopped, &mut spent)?);
        } else {
            let mut unproven = Vec::new();
            for (translation, law, outcome) in self.run(work, Form::Steps, &mut stopped, &mut spent)? {
                let settled = matches!(outcome, Ok(Judgment::Proved) | Err(_)) || self.unproven_call(law).is_some();
                if settled {
                    ended.push((translation, law, outcome));
                } else {
                    tracing::debug!("The fixed steps did not prove law {}: {outcome:?}", law.name);
                    unproven.push((translation, law));
                }
            }
            tracing::debug!(
                "{} laws end with the fixed steps, and {} go on to Lean's search.",
                ended.len(),
                unproven.len()
            );
            ended.extend(self.run(by_law(unproven), Form::Search, &mut stopped, &mut spent)?);
        }
        ended.sort_by_key(|(_, law, _)| order[law.statement.as_str()]);
        let mut slowest: Vec<(&str, (Duration, u64))> = spent.into_iter().collect();
        slowest.sort_unstable_by(|(law, (_, heartbeats)), (other, (_, other_heartbeats))| {
            other_heartbeats.cmp(heartbeats).then_with(|| law.cmp(other))
        });
        let outcomes: HashMap<&str, &'static str> = ended
            .iter()
            .map(|(_, law, ended)| {
                let outcome = match ended {
                    Ok(Judgment::Proved) => "proved",
                    Ok(Judgment::Open(..)) => "unproven",
                    Ok(Judgment::Through(_)) => "unproven through another law",
                    Ok(Judgment::Rejected(_)) => "rejected",
                    Ok(Judgment::OutOfTime) => "out of time",
                    Err(_) => "refused",
                };
                (law.statement.as_str(), outcome)
            })
            .collect();
        for (law, (took, heartbeats)) in slowest {
            tracing::debug!(
                "The law {} ended {} after {:.2} s and {heartbeats} thousand heartbeats of the provers' time.",
                names::unescaped(law),
                outcomes.get(law).unwrap_or(&"unjudged"),
                took.as_secs_f64(),
            );
        }

        Ok(ended)
    }

    /// Proves the laws of `work` with their theorems in the `form` form, in one runner after another, and gives how
    /// each law's proof ended. Each entry's laws are elaborated with the theorem of each law they call, transitively,
    /// before them. A law that calls a law that cannot be proven is not proven. A generated module Lean does not
    /// elaborate refuses each law of the check that reaches it, and the next runner checks the entry's other laws
    /// without it. Each law's time and heartbeats in the runners join its own in `spent`, and each runner logs how long
    /// it took.
    fn run<'work>(
        &self,
        work: Vec<(&'work Translation, Vec<&'work Law>)>,
        form: Form,
        stopped: &mut Vec<(String, Stop)>,
        spent: &mut HashMap<&'work str, (Duration, u64)>,
    ) -> io::Result<Vec<Proven<'work>>> {
        let mut ended: Vec<Proven<'work>> = Vec::new();
        let mut pending = work;
        while !pending.is_empty() {
            // One runner answers the checks of every file at once, so Lean and the runtime library are imported once per
            // run.
            let (blocked, checks): (Vec<_>, Vec<_>) =
                pending.iter().map(|(translation, laws)| self.check(translation, laws, form)).unzip();
            let modules = self.reached_modules(&checks);
            let running = Instant::now();
            let answers = runner::check(self.library, &modules, &checks, self.threads, HEARTBEATS, stopped)?;
            tracing::debug!(
                "A runner proved {} laws with {} in {:.2} s.",
                checks.iter().flat_map(|check| &check.theorems).filter(|theorem| theorem.judged).count(),
                form.steps(),
                running.elapsed().as_secs_f64(),
            );
            let mut left = Vec::new();
            for ((((translation, laws), check), blocked), answer) in
                pending.into_iter().zip(&checks).zip(blocked).zip(answers)
            {
                match answer {
                    Answer::Judged(answered) => {
                        ended.extend(
                            blocked.into_iter().map(|(index, judgment)| (translation, laws[index], Ok(judgment))),
                        );
                        let order =
                            check.theorems.iter().filter(|theorem| theorem.judged).map(|theorem| theorem.statement);
                        for (statement, judged) in order.zip(answered) {
                            if let Some(&law) = laws.iter().find(|law| law.statement == statement) {
                                let (took, heartbeats) = spent.entry(law.statement.as_str()).or_default();
                                *took += judged.took;
                                *heartbeats += u64::from(judged.heartbeats);
                                ended.push((translation, law, Ok(judged.judgment)));
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
                            ended.push((translation, law, Err((module.clone(), refusal))));
                        }
                        if !kept.is_empty() {
                            left.push((translation, kept));
                        }
                    }
                }
            }
            pending = left;
        }

        Ok(ended)
    }

    /// Each generated module `checks` import, and each module those import, transitively, in the order of their
    /// names: all a runner elaborates for them.
    fn reached_modules(&self, checks: &[Check<'_>]) -> Vec<&package::Module> {
        let mut reached: BTreeMap<&str, &package::Module> = BTreeMap::new();
        let mut pending: Vec<&str> = checks.iter().flat_map(|check| &check.modules).map(String::as_str).collect();
        while let Some(name) = pending.pop() {
            if let Some(module) = self.modules.get(name)
                && reached.insert(module.name.as_str(), module).is_none()
            {
                pending.extend(module.imports.iter().map(String::as_str));
            }
        }

        reached.into_values().collect()
    }

    /// Whether `law` reaches the generated `module`: the module of the law, or of a law it calls, transitively, is that
    /// module or imports it, transitively.
    fn reaches(&self, law: &Law, module: &str) -> bool {
        let hosts =
            self.laws.closure(law).into_iter().map(|position| self.host_of(self.laws.ordered[position].0)).collect();

        imports_reach(hosts, &self.modules, module)
    }

    /// The law `law` calls, transitively, that cannot be proven: one in a cycle of laws, or one whose code cannot be
    /// written to Lean.
    fn unproven_call(&self, law: &Law) -> Option<&Law> {
        let ordered = self.laws;

        ordered.closure(law).into_iter().map(|position| ordered.ordered[position].1).find(|called| {
            called.statement != law.statement
                && (ordered.cyclic.contains(called.statement.as_str())
                    || !self.program.problems(&called.statement).is_empty())
        })
    }

    /// How `laws`, of `translation`, are judged without the runner, each by its index in `laws` and through a law it
    /// calls that cannot be proven, and the check the runner judges the others with, their theorems in the `form` form.
    /// The check reaches the file's own generated module even when no theorem is left to elaborate, so a module Lean
    /// rejects refuses the file's laws.
    fn check(&self, translation: &Translation, laws: &[&Law], form: Form) -> (Vec<(usize, Judgment)>, Check<'_>) {
        let ordered = self.laws;
        let mut theorems: Vec<usize> = Vec::new();
        let mut judgments: Vec<(usize, Judgment)> = Vec::new();
        for (index, &law) in laws.iter().enumerate() {
            match self.unproven_call(law) {
                Some(called) => judgments.push((index, Judgment::Through(called.statement.clone()))),
                None => theorems.extend(ordered.closure(law)),
            }
        }
        theorems.sort_unstable();
        theorems.dedup();

        let blocked: HashSet<&str> = judgments.iter().map(|(index, _)| laws[*index].statement.as_str()).collect();
        let judged: HashSet<&str> =
            laws.iter().map(|law| law.statement.as_str()).filter(|statement| !blocked.contains(statement)).collect();
        let mut modules: Vec<String> = theorems
            .iter()
            .map(|position| self.host_of(ordered.ordered[*position].0))
            .chain(std::iter::once(self.host_of(translation)))
            .collect();
        let mut searches: HashMap<&str, &str> = HashMap::default();
        for law in theorems.iter().map(|position| ordered.ordered[*position].1) {
            if judged.contains(law.statement.as_str())
                && let Some((text, hosts)) = self.search(law)
            {
                modules.extend(hosts);
                searches.insert(law.statement.as_str(), text);
            }
        }
        modules.sort();
        modules.dedup();
        modules.push(
            match form {
                Form::Search | Form::StepsThenSearch => PROOF_STEPS,
                Form::Steps => LAW_STATEMENT,
            }
            .to_owned(),
        );
        let theorems: Vec<Theorem<'_>> = theorems
            .iter()
            .map(|position| {
                let law = ordered.ordered[*position].1;
                Theorem {
                    statement: &law.statement,
                    text: &law.proof.text,
                    steps: match form {
                        Form::Search => None,
                        Form::Steps | Form::StepsThenSearch => {
                            self.scripts.get(law.statement.as_str()).map(String::as_str)
                        }
                    },
                    judged: judged.contains(law.statement.as_str()),
                    search: searches.get(law.statement.as_str()).copied(),
                }
            })
            .collect();

        (judgments, Check { modules, theorems })
    }
}

/// The issues of `law` the runner's `judgment` gives: one for each case and each call whose goal it left open, with
/// the goal in PHP#, which names the code as `names` holds it.
fn judged(law: &Law, judgment: Judgment, names: &Names<'_>) -> Vec<Issue> {
    match judgment {
        Judgment::Proved => Vec::new(),
        Judgment::Through(called) => vec![issues::through(law, &called)],
        Judgment::OutOfTime => vec![issues::theorem_out_of_time(law)],
        Judgment::Rejected(error) => {
            tracing::debug!("The law checker rejected the proof of law {}: {error}", law.name);

            vec![issues::rejected_proof(law)]
        }
        // Spec section 20.2: an invariant of a machine is broken by the moves its trace search found, and else by each
        // entry point or move whose part of its proof stays open, with the goal still to show.
        Judgment::Open(steps, _) if !law.proof.after.is_empty() => {
            let found = steps.iter().find(|step| step.index > law.proof.after.len()).and_then(|step| {
                let Ok(serde_json::Value::Array(trace)) = serde_json::from_str(&step.goal) else {
                    return None;
                };
                let moves: Vec<String> =
                    trace.iter().skip(1).filter_map(|step| Some(format!("`{}`", step.as_str()?))).collect();

                Some(moves.join(", "))
            });
            match found {
                Some(trace) => vec![issues::broken_invariant(law, &trace)],
                None => steps
                    .iter()
                    .filter_map(|step| Some((step, law.proof.after.get(step.index - 1)?)))
                    .map(|(step, after)| {
                        let (split, goal) = goal::case(&step.goal, names);
                        let issue = issues::unproven_invariant(law, after, &split, goal.as_deref());

                        if step.out_of_time { issues::out_of_time(issue) } else { issue }
                    })
                    .collect(),
            }
        }
        // A case a `match` reaches both where a guard fails and where a pattern does is one case, reported once. A case
        // of the enums a law without a body splits on is a case of its own. Each says what the search for the small
        // values that break the law gave.
        Judgment::Open(steps, search) => {
            let mut reported: Vec<(usize, bool, Vec<String>)> = Vec::new();
            let mut issues = Vec::new();
            for step in &steps {
                let (issue, split) = if step.condition {
                    let call = &law.proof.calls[step.index - 1];
                    let names = Names { arguments: &call.arguments, ..*names };
                    (issues::unshown_condition(law, call, goal::conjunction(&step.goal, &names).as_deref()), Vec::new())
                } else {
                    let case = &law.proof.cases[step.index - 1];
                    let (split, goal) = goal::case(&step.goal, names);
                    (issues::unproven_case(law, case, &split, goal.as_deref()), split)
                };
                let key = (step.index, step.condition, split);
                if reported.contains(&key) {
                    continue;
                }
                reported.push(key);
                let issue = if step.out_of_time { issues::out_of_time(issue) } else { issue };
                issues.push(issues::searched(law, issue, &search));
            }

            issues
        }
    }
}

/// Records `issue` on the file of its primary annotation: the file of `translation`, which states the law, or the file
/// of the PHP# implementer whose proof of an interface's law it reports, spec sections 22 and 28.
fn report(issues: &mut HashMap<FileId, IssueCollection>, translation: &Translation, issue: Issue) {
    let file = issue
        .annotations
        .iter()
        .find(|annotation| annotation.is_primary())
        .map_or(translation.file, |annotation| annotation.span.file_id);
    issues.entry(file).or_default().push(issue);
}

/// The issues that refuse the laws of `translations` before any proof, as [`Lean::check`] reports them.
///
/// Each issue is in the file that states its law, and no Lean process starts. `translations` hold each file whose laws
/// are checked and each file those laws reach but the files of `errored`, those whose analysis reported an error.
#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn refusals(translations: &[Translation], errored: &HashSet<FileId>) -> HashMap<FileId, IssueCollection> {
    let program = Program::new(translations);
    let laws = Laws::new(translations);
    let mut issues: HashMap<FileId, IssueCollection> = HashMap::default();
    for translation in translations {
        for law in &translation.laws {
            for issue in refused(law, &program, &laws, errored) {
                report(&mut issues, translation, issue);
            }
        }
    }

    issues
}

/// The issues that refuse `law` before any proof: each construct it reaches that the translation does not model, each
/// declaration it reaches and each law it calls that no translation holds, for the errors of its file when its file is
/// in `errored` or unknown, and each call it makes in a cycle of laws. A law with none is proved.
fn refused(law: &Law, program: &Program<'_>, laws: &Laws<'_>, errored: &HashSet<FileId>) -> Vec<Issue> {
    let untranslated = |place: &str, file: Option<FileId>| {
        if file.is_none_or(|file| errored.contains(&file)) {
            issues::refused_reach(law, place)
        } else {
            issues::untranslated(law, place)
        }
    };
    let problems = program.problems(&law.statement);
    // A construct reached in two places of another file stands on the law twice with one message, and prints once.
    let mut printed = HashSet::default();
    let mut issues: Vec<Issue> = problems
        .unmodeled
        .iter()
        .map(|construct| issues::unmodeled(law, construct))
        .filter(|issue| printed.insert((issue.message.clone(), issue.primary_span())))
        .collect();
    issues.extend(problems.missing.iter().map(|missing| untranslated(&missing.place, missing.file)));
    issues.extend(
        law.proof
            .laws
            .iter()
            .filter(|(called, ..)| !laws.position.contains_key(called.as_str()))
            .map(|(called, _, file)| untranslated(&place_of(called), *file)),
    );
    if laws.cyclic.contains(law.statement.as_str()) {
        for (called, span, _) in law.proof.laws.iter().filter(|(called, ..)| laws.cyclic.contains(called.as_str())) {
            issues.push(issues::law_cycle(*span, called));
        }
    }

    issues
}

/// Whether one of the generated modules `from` is `module`, or imports it, transitively, through the imports `modules`
/// hold.
fn imports_reach(mut from: Vec<String>, modules: &HashMap<&str, &package::Module>, module: &str) -> bool {
    let mut seen: HashSet<String> = HashSet::default();
    while let Some(next) = from.pop() {
        if next == module {
            return true;
        }
        if let Some(found) = modules.get(next.as_str()) {
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

    fn law() -> Law {
        Law {
            name: "addKeepsCurrency".to_owned(),
            statement: "App.Shared.Money.addKeepsCurrency".to_owned(),
            span: Span::dummy(0, 0),
            proof: crate::translate::proof::Proof::default(),
            breaking: None,
            broken: None,
        }
    }

    /// A law a body calls that no translation states refuses the law for the errors of its file when the analysis
    /// reported errors in that file or doesn't know the file, and as the checker's bug when the file has none.
    #[test]
    fn a_called_law_no_translation_states_is_refused_for_its_file_s_errors_only_when_its_file_has_errors() {
        let broken = FileId::new(b"app/Shared/Broken.sharp");
        let mut calling = law();
        calling.proof.laws = vec![
            ("App.Shared.Broken.holds".to_owned(), Span::dummy(0, 0), Some(broken)),
            ("App.Shared.Clean.holds".to_owned(), Span::dummy(0, 0), Some(FileId::new(b"app/Shared/Clean.sharp"))),
            ("App.Shared.Unknown.holds".to_owned(), Span::dummy(0, 0), None),
        ];
        let translation = Translation {
            file: FileId::new(b"app/Shared/Money.sharp"),
            name: b"app/Shared/Money.sharp".to_vec(),
            class: None,
            laws: vec![calling],
            declarations: Vec::new(),
            receivers: Vec::new(),
        };

        let refused = refusals(&[translation], &HashSet::from_iter([broken]));

        assert_eq!(
            refused.values().flat_map(|issues| issues.iter()).map(|issue| issue.message.as_str()).collect::<Vec<_>>(),
            [
                "Law addKeepsCurrency is not proven: it reaches Broken.holds, whose file has errors. Fix them, then run vendor/bin/mago compile again.",
                "Law addKeepsCurrency is not proven: the checker left out Clean.holds, which the law reaches. This is a bug in the PHP# checker; report it to its maintainers.",
                "Law addKeepsCurrency is not proven: it reaches Unknown.holds, whose file has errors. Fix them, then run vendor/bin/mago compile again.",
            ]
        );
    }

    /// A proof or a module the checker wrote wrong is the checker's bug, which the message says in PHP#, naming no Lean
    /// module and quoting no Lean error.
    #[test]
    fn a_proof_or_a_module_the_checker_wrote_wrong_names_no_lean() {
        let (receivers, declared, sealed) = (HashMap::default(), HashSet::default(), HashSet::default());
        let rejected = judged(
            &law(),
            Judgment::Rejected("unknown identifier 'Code.App.Shared.Money.add'".to_owned()),
            &Names { receivers: &receivers, declared: &declared, sealed: &sealed, arguments: &[] },
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
        let by_name: HashMap<&str, &package::Module> =
            modules.iter().map(|module| (module.name.as_str(), module)).collect();
        let reaches = |from: &[&str]| {
            imports_reach(from.iter().map(|host| (*host).to_owned()).collect(), &by_name, "Code.App.Money")
        };

        assert!(reaches(&["Code.App.Money"]));
        assert!(reaches(&["Code.App.Clock", "Code.App.Bank"]));
        assert!(!reaches(&["Code.App.Clock"]));
        assert!(!reaches(&[]));
    }

    /// A recorded proof stands for the law while its key holds, and the record drops a proof whose law's file states no
    /// law any more or whose law has a new key.
    #[test]
    fn the_record_keeps_each_proof_of_a_law_file_whose_key_holds() {
        let recorded = |file: &str, statement: &str| Recorded {
            file: file.to_owned(),
            statement: statement.to_owned(),
            judgment: Judgment::Proved,
        };
        let mut record = Record::default();
        record.proofs.insert("kept".to_owned(), recorded("app/Money.sharp", "App.Money.kept"));
        record.proofs.insert("old".to_owned(), recorded("app/Money.sharp", "App.Money.edited"));
        record.proofs.insert("gone".to_owned(), recorded("app/Gone.sharp", "App.Gone.law"));
        let law_files: HashSet<FileId> = HashSet::from_iter([FileId::new(b"app/Money.sharp")]);
        let keys: HashMap<&str, String> = HashMap::from_iter([("App.Money.edited", "new".to_owned())]);

        record.keep(&law_files, &keys);

        assert_eq!(record.proofs.keys().collect::<Vec<_>>(), ["kept"]);
    }

    /// A law's key names the library's sources, the heartbeats each theorem may use and the law's fixed steps beside its
    /// texts: a change to any of them proves the law again, and a release that changes none of them, as one that
    /// changes only Mago's version, keeps its outcome.
    #[test]
    fn a_law_s_key_changes_with_the_library_the_heartbeats_and_the_fixed_steps_alone() {
        let translations = [Translation {
            file: FileId::new(b"app/Shared/Money.sharp"),
            name: b"app/Shared/Money.sharp".to_vec(),
            class: None,
            laws: vec![law()],
            declarations: Vec::new(),
            receivers: Vec::new(),
        }];
        let (program, laws) = (Program::new(&translations), Laws::new(&translations));
        let key = |sources: &str, heartbeats: u32, steps: &str| {
            let scripts = HashMap::from_iter([("App.Shared.Money.addKeepsCurrency", steps.to_owned())]);

            proof_key(&translations[0].laws[0], &program, &laws, &scripts, &settings(sources, heartbeats))
        };
        let sources = library::sources();
        let edited = format!("{sources}0");

        let base = key(&sources, HEARTBEATS, "simp");

        assert_eq!(base, key(&sources, HEARTBEATS, "simp"));
        assert_ne!(base, key(&edited, HEARTBEATS, "simp"), "a change to the library");
        assert_ne!(base, key(&sources, HEARTBEATS + 1, "simp"), "a change to the heartbeats");
        assert_ne!(base, key(&sources, HEARTBEATS, "omega"), "a change to the fixed steps");
    }
}
