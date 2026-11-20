//! `mago convert <paths>`: raises each plain PHP class file the paths name into PHP#, in place, as `Foo.php` becoming
//! `Foo.sharp`, or names each construct that keeps it plain PHP.
//!
//! The types come from the project's own analysis, run as `mago analyze` runs it. Before it writes a file, the command
//! analyzes the converted text against the whole project and compiles it in memory, and keeps the file plain PHP when
//! the converted file has any error or warning, another file gains an error or a warning decision 9 doesn't expect, or
//! the compile refuses a file it accepted before. The project's `ignore` entries and baseline hide an extension host
//! rule's issue there, as `mago compile` hides it, and never one of the language's (spec section 27). The extension
//! hosts load the project's classes, so each file it writes that their compile takes in starts the analysis again. Git
//! is the undo: a file converts only as the last commit holds it, a git that can't read the repository stops the run
//! with git's reason, and the command records each rename in the index as `git mv` does, so the index holds the rename
//! alone, git's history follows the file through it, and the `.php` file `mago compile` later writes at the old path is
//! never the tracked one. A file's shape and git decide before any analysis, so a run with nothing to prove runs none.
//!
//! A converted file that reaches a library's function, constant or class of PHP's root namespace needs an
//! `extern namespace` line for its folder (spec section 23), and one that reaches plain PHP no `extern` line declares
//! needs a line for it, which the command writes `uses any` (spec section 29). The command owns one declarations file
//! for them, `Stubs/Externs.sharp` in the first PSR-4 folder, in that folder's `Stubs` namespace: it creates the file or
//! adds to it, one line for each folder, class and function, sorted, with the imports its lines need, and each proof
//! holds the lines its converted files need. A run writes it before the converted files and never stages it, as it
//! never stages a converted file's new text, so the index holds only the renames.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::ColorChoice;
use clap::Parser;
use diffy::PatchFormatter;

use mago_allocator::LocalArena;
use mago_analyzer::effects::Effects;
use mago_codex::metadata::CodebaseMetadata;
use mago_composer::AutoloadPsr4value;
use mago_composer::ComposerPackage;
use mago_converter::Converted;
use mago_converter::Input;
use mago_converter::Library;
use mago_converter::Refusal;
use mago_converter::Undeclared;
use mago_converter::convert;
use mago_converter::is_reserved_alias;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::loader::mark_sharp_file;
use mago_database::matcher::ExclusionMatcher;
use mago_database::matcher::contains_glob_metacharacters;
use mago_names::resolver::NameResolver;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_orchestrator::service::incremental_analysis::compile::CompiledFiles;
use mago_orchestrator::service::incremental_analysis::compile::Folder;
use mago_orchestrator::service::incremental_analysis::compile::LOWERING_PANIC;
use mago_reporting::CompiledIgnoreSet;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_reporting::baseline::Baseline;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;

use crate::baseline::unserialize_baseline;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::compile::HostsChange;
use crate::commands::compile::compile_for_hosts;
use crate::commands::compile::project_database;
use crate::commands::compile::standard_library;
use crate::commands::compile::whole_workspace;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::error::Error;
use crate::extensions::start_external_analyzer;
use crate::utils::git;
use crate::utils::should_use_colors;
use crate::utils::write_by_rename;

/// Command that raises plain PHP class files into PHP#.
#[derive(Parser, Debug)]
#[command(name = "convert")]
pub struct ConvertCommand {
    /// The plain PHP files, and folders of them, to convert.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Print each conversion as a diff, and change no file.
    #[arg(long, short = 'd')]
    pub dry_run: bool,
}

/// The warnings a conversion adds by design, decision 9 of `docs/architecture/slices/convert.md`: a plain PHP caller's
/// PHPDoc type that the converted class now declares itself.
const EXPECTED_WARNINGS: [&str; 1] = ["redundant-docblock-type"];

/// An issue as a later analysis finds it again: its file, level, code, message and line.
type IssueKey = (Vec<u8>, Level, Option<String>, String, u32);

/// Each file shape and git let convert, by its name, with the bytes git checked.
type Checked = Vec<(Vec<u8>, Vec<u8>)>;

/// Each issue of an analysis, with how many times it is reported, the files its annotations point into, and the
/// members of a PHP# file it is in, as [`members_of`] names them, the innermost first.
type Issues = HashMap<IssueKey, (usize, BTreeSet<Vec<u8>>, Vec<String>)>;

/// What the project's configuration hides of an extension host rule's issues, as `mago compile` and `mago analyze`
/// hide it, and never of the language's own (spec section 27).
struct Hidden {
    ignored: CompiledIgnoreSet,
    baseline: Option<Baseline>,
}

impl Hidden {
    fn new(configuration: &Configuration) -> Result<Self, Error> {
        let ignored = CompiledIgnoreSet::compile(
            &configuration.analyzer.ignore,
            configuration.source.glob.to_database_settings(),
        );
        let baseline = match configuration.analyzer.baseline.as_deref().filter(|path| path.exists()) {
            Some(path) => Some(unserialize_baseline(path)?.0),
            None => None,
        };

        Ok(Self { ignored, baseline })
    }

    /// `issues` of the files of `database` without each host rule's issue an `ignore` entry or the baseline hides.
    fn shown(&self, issues: IssueCollection, database: &ReadDatabase) -> IssueCollection {
        let (host, language): (Vec<_>, Vec<_>) =
            issues.into_iter().partition(|issue| issue.code.as_deref().is_some_and(|code| code.contains('/')));
        let mut host = IssueCollection::from(host);
        host.filter_out_ignored(&self.ignored, |file_id| {
            database.get_ref(&file_id).ok().map(|file| String::from_utf8_lossy(&file.name).into_owned())
        });
        if let Some(baseline) = &self.baseline {
            host = baseline.filter_issues(host, database);
        }

        let mut shown = IssueCollection::from(language);
        shown.extend(host);
        shown
    }
}

/// A plain PHP file the converter turned into PHP#, before its proof.
struct Candidate {
    name: Vec<u8>,
    path: Option<PathBuf>,
    original: Vec<u8>,
    text: Vec<u8>,
    /// The `extern namespace` lines it needs, each folder's as the run's first file that needs one names it.
    extern_namespaces: Vec<String>,
    /// The plain PHP it reaches that no `extern` line declares.
    undeclared: Vec<Undeclared>,
    /// The full names of the classes it declares, as PHP writes them, which turn PHP# with it.
    declares: Vec<String>,
}

/// The project's declarations file the command owns, `Stubs/Externs.sharp` in the first PSR-4 folder, in that folder's
/// `Stubs` namespace, which holds the `extern namespace` lines of the converted files (spec section 23) and the
/// `uses any` line of each plain PHP class and function they reach that no line declares (spec section 29).
struct Externs {
    /// Its name from the workspace, as `src/Stubs/Externs.sharp`.
    name: Vec<u8>,
    path: PathBuf,
    /// The namespace a new file declares, as `App.Stubs`.
    namespace: String,
    /// The text the file held when the run started, empty when it was missing.
    held: Vec<u8>,
}

/// What one analysis and in-memory compile of the project found: each issue, and each file `mago compile` refuses,
/// with the errors that refuse it.
struct Proof {
    issues: Issues,
    refused: HashMap<Vec<u8>, IssueCollection>,
}

/// What a proof found that the plain project's didn't: each converted file's own issues, by its plain name, the other
/// files' issues, and the warnings decision 9 expects.
#[derive(Default)]
struct Judgement {
    own: BTreeMap<Vec<u8>, Vec<Found>>,
    others: Vec<Found>,
    expected: Vec<Found>,
}

/// An issue a proof found, as the summary shows it, with the files it points into: the files of its annotations, or
/// the file `mago compile` refuses. An issue of a converted file shows at its plain file while the converted one is
/// not written, as in a dry run and in a file that stays plain PHP, and at the `.sharp` file once it is.
struct Found {
    shown: String,
    written: String,
    files: BTreeSet<Vec<u8>>,
}

/// What every proof of one command shares: the project's configuration, what it hides of a host rule's issues, and
/// the standard library its compile lowers against.
struct Prover<'configuration> {
    configuration: &'configuration Configuration,
    color_choice: ColorChoice,
    root: PathBuf,
    hidden: Hidden,
    standard_library: unit::Library,
    externs: Externs,
}

/// One analysis of the project with fresh extension hosts, as `mago analyze` runs it, with converted files in place of
/// their plain ones. Fresh hosts every time make each proof comparable with the plain project's: hosts that analyze
/// the same project twice report what the first pass left behind.
struct Session<'configuration> {
    /// Keeps the extension hosts the server analyzes with running.
    _orchestrator: Orchestrator<'configuration>,
    server: Server,
}

/// What the conversions of the command found.
#[derive(Default)]
struct Outcome {
    /// Why each path the command line names reaches no plain PHP file the project's analysis loads.
    unreached: Vec<String>,
    refused: BTreeMap<Vec<u8>, Vec<String>>,
    converted: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    /// The declarations file the converted files need lines in, its text before, and its text with them.
    externs: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    /// The warnings decision 9 expects.
    expected: Vec<Found>,
    /// How many proofs of the converted files ran, each a whole analysis of the project with fresh extension hosts.
    proofs: usize,
    /// Whether the run itself failed: a proof could not finish, the written files' proof did not hold, a write failed
    /// or a lowering panicked. A file that stays plain PHP fails nothing, since the report says why.
    failed: bool,
}

impl ConvertCommand {
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
        Ok(self.run(configuration, color_choice)?.0.into())
    }

    /// Converts the files the paths name and reports it, with the exit code and what the run found.
    fn run(self, configuration: Configuration, color_choice: ColorChoice) -> Result<(ExitCode, Outcome), Error> {
        let (outcome, checked) = self.check(&configuration, color_choice)?;

        self.convert_checked(configuration, color_choice, outcome, checked)
    }

    /// Each plain PHP file the paths name that its shape and git let convert, by its name with the bytes git holds,
    /// and what keeps the others plain PHP and why a path reaches no file. It runs before any analysis, so a run with
    /// nothing to prove starts no host. Shape decides first, so a script is named a script without git.
    fn check(&self, configuration: &Configuration, color_choice: ColorChoice) -> Result<(Outcome, Checked), Error> {
        let workspace = &configuration.source.workspace;
        let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
        let roots = psr4_roots(workspace)?;
        let named: Vec<PathBuf> = self.paths.iter().map(|path| relative(workspace, path)).collect();
        let (database, _) = project_database(&whole_workspace(configuration, color_choice, false)?, workspace)?;

        let mut outcome = Outcome::default();
        let mut refuse = |name: &[u8], reason: String| {
            outcome.refused.entry(name.to_vec()).or_default().push(format!("{}: {reason}", display(name)));
        };
        let mut reached: HashSet<&PathBuf> = HashSet::new();
        let mut shaped = Vec::new();
        for file in database.files() {
            let name = Path::new(std::str::from_utf8(&file.name).unwrap_or_default());
            let mut naming = named.iter().filter(|path| name.starts_with(path)).peekable();
            if file.file_type != FileType::Host || !file.name.ends_with(b".php") || naming.peek().is_none() {
                continue;
            }
            reached.extend(naming);
            match shape(&file, &roots) {
                Ok(()) => shaped.push(file),
                Err(reason) => refuse(&file.name, reason),
            }
        }
        shaped.sort_by(|a, b| a.name.cmp(&b.name));

        // Git is the undo, so a file converts only as git holds it.
        let mut checked = Vec::new();
        if !shaped.is_empty() {
            let committed = git::committed_files(workspace)?;
            for file in shaped {
                let name = PathBuf::from(display(&file.name));
                if let Some(submodule) = committed.submodules.iter().find(|submodule| name.starts_with(submodule)) {
                    let reason = format!(
                        "it is in the git submodule `{}`, whose files another repository holds, so the workspace's commit holds none of its plain PHP",
                        submodule.display()
                    );
                    refuse(&file.name, reason);
                } else if committed.files.contains(&name) {
                    checked.push((file.name.to_vec(), file.contents.to_vec()));
                } else {
                    refuse(
                        &file.name,
                        "git holds no commit of it as it is, so git, which undoes a conversion, can't give back its plain PHP"
                            .to_owned(),
                    );
                }
            }
        }
        outcome.unreached = named
            .iter()
            .filter(|path| !reached.contains(path))
            .map(|path| unreached(configuration, &root, path))
            .collect::<Result<_, _>>()?;

        Ok((outcome, checked))
    }

    /// Proves and converts `checked`, the files [`Self::check`] let convert, each by its name with the bytes git holds,
    /// and reports `outcome` with what they add to it.
    fn convert_checked(
        self,
        mut configuration: Configuration,
        color_choice: ColorChoice,
        mut outcome: Outcome,
        checked: Checked,
    ) -> Result<(ExitCode, Outcome), Error> {
        // With no file to convert, the run is done: each file it reached stays plain PHP for its reason.
        if checked.is_empty() {
            outcome.report(self.dry_run, color_choice);
            return Ok((outcome.exit_code(), outcome));
        }

        let workspace = configuration.source.workspace.clone();
        let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
        // Every file that converts is under a PSR-4 folder, as `shape` keeps it, so the first one always exists.
        let Some(externs) = psr4_roots(&workspace)?.first().map(|root| Externs::new(&workspace, root)).transpose()?
        else {
            outcome.report(self.dry_run, color_choice);
            return Ok((outcome.exit_code(), outcome));
        };
        configuration.extension_hosts = compile_for_hosts(&configuration)?.0;
        let Some((prover, mut session)) = Prover::new(&configuration, color_choice, root, externs)? else {
            outcome.report(self.dry_run, color_choice);
            return Ok((ExitCode::FAILURE, outcome));
        };
        let plain = session.proof(&prover)?;
        // The judgement leaves out a file the plain project's compile refuses too, so a lowering that panicked there,
        // a defect of the checker, is an error of the run of its own.
        let panics = panics(&plain);
        for panic in &panics {
            tracing::error!("{panic}");
        }
        // The forms come from the library files the proof's compile accepts, so a converted call runs what it does.
        let database = session.server.database().read_only();
        let forms = session.server.standard_library_forms(|issues| prover.hidden.shown(issues, &database))?;
        drop(database);
        let library = Library::new(&forms, session.server.codebase(), &workspace);
        let effects = session.server.effects();

        let mut candidates = Vec::new();
        // One line gives a folder its namespace, so each folder's is the one the first file that needs it names.
        let mut folder_lines: HashMap<String, String> = HashMap::new();
        for (name, held) in checked {
            // A file changed or gone since git was read holds bytes git doesn't, so it is no longer git's to undo.
            let database = session.server.database();
            let Some(file) = database.get_ref(&FileId::new(&name)).ok().filter(|file| *file.contents == *held) else {
                let reason = format!(
                    "{}: it changed since mago convert read git, so git, which undoes a conversion, can't give back its plain PHP",
                    display(&name)
                );
                outcome.refused.entry(name).or_default().push(reason);
                continue;
            };
            match session.convert(file, &library, &effects) {
                Ok(Converted { text, extern_namespaces, undeclared }) => {
                    let declares = session
                        .server
                        .codebase()
                        .class_likes
                        .values()
                        .filter(|class| class.span.file_id == file.id)
                        .map(|class| class.original_name.as_str_lossy().trim_start_matches('\\').to_owned())
                        .collect();
                    let extern_namespaces = extern_namespaces
                        .iter()
                        .map(|needed| {
                            folder_lines.entry(needed.folder.clone()).or_insert_with(|| needed.line()).clone()
                        })
                        .collect();
                    candidates.push(Candidate {
                        name,
                        path: file.path.clone(),
                        original: file.contents.to_vec(),
                        text,
                        extern_namespaces,
                        undeclared,
                        declares,
                    });
                }
                Err(reasons) => outcome.refused.entry(name).or_default().extend(reasons),
            }
        }
        drop(session);

        let mut accepted = Vec::new();
        prover.prove(&plain, &candidates, &mut accepted, (0..candidates.len()).collect(), &mut outcome);
        let accepted: Vec<&Candidate> = accepted.into_iter().map(|index| &candidates[index]).collect();
        outcome.converted = accepted
            .iter()
            .map(|candidate| (candidate.name.clone(), candidate.original.clone(), candidate.text.clone()))
            .collect();
        outcome.externs =
            prover.externs.text(&accepted).map(|(before, after)| (prover.externs.name.clone(), before, after));

        // A lowering that panicked is a defect of the checker, which fails the run.
        outcome.failed |= !panics.is_empty();
        if !self.dry_run && !accepted.is_empty() {
            // The declarations file goes first, so no converted file is on disk without the lines it needs.
            if let Some((name, _, text)) = &outcome.externs
                && let Err(error) = write_by_rename(&prover.externs.path, text)
            {
                outcome.failed = true;
                let name = display(name);
                tracing::error!("{name} could not be written, so every file stays plain PHP: {error}");
                outcome.converted.clear();
                outcome.externs = None;
                outcome.report(self.dry_run, color_choice);

                return Ok((outcome.exit_code(), outcome));
            }
            let files: Vec<(&[u8], &[u8])> =
                accepted.iter().map(|candidate| (candidate.name.as_slice(), candidate.text.as_slice())).collect();
            let (written, stopped) = write(&workspace, &files);
            if let Some(error) = stopped {
                outcome.failed = true;
                let name = display(&sharp_name(&accepted[written].name));
                tracing::error!("{name} could not be written, so it and the files after it stay plain PHP: {error}");
            }
            outcome.converted.truncate(written);
            if written > 0 && !prover.prove_written(&plain, &accepted[..written]) {
                outcome.failed = true;
            }
        }

        outcome.report(self.dry_run, color_choice);

        Ok((outcome.exit_code(), outcome))
    }
}

impl<'configuration> Prover<'configuration> {
    /// The prover of the project, and a session of the plain project. `None` when the standard library's package can't
    /// be found, which the log names.
    fn new(
        configuration: &'configuration Configuration,
        color_choice: ColorChoice,
        root: PathBuf,
        externs: Externs,
    ) -> Result<Option<(Self, Session<'configuration>)>, Error> {
        let hidden = Hidden::new(configuration)?;
        let (orchestrator, database) = load(configuration, color_choice)?;
        let standard_library = match standard_library(&root, &database) {
            Ok(library) => library,
            Err(reason) => {
                tracing::error!("{reason}");
                return Ok(None);
            }
        };
        let prover = Self { configuration, color_choice, root, hidden, standard_library, externs };
        let session = Session::new(orchestrator, database);

        Ok(Some((prover, session)))
    }

    /// A session of the project with fresh extension hosts, each of `swaps` in place of its plain file, and the
    /// declarations file with the lines they need.
    fn open(&self, swaps: &[&Candidate]) -> Result<Session<'configuration>, Error> {
        let (orchestrator, mut database) = load(self.configuration, self.color_choice)?;
        if let Some((_, text)) = self.externs.text(swaps) {
            database.delete(FileId::new(&self.externs.name));
            let mut externs = File::new(
                Cow::Owned(self.externs.name.clone()),
                FileType::Host,
                Some(self.externs.path.clone()),
                Cow::Owned(text),
            );
            mark_sharp_file(&mut externs, &self.configuration.source.workspace);
            database.add(externs);
        }
        for candidate in swaps {
            database.delete(FileId::new(&candidate.name));
            let mut converted = File::new(
                Cow::Owned(sharp_name(&candidate.name)),
                FileType::Host,
                candidate.path.as_ref().map(|path| path.with_extension("sharp")),
                Cow::Owned(candidate.text.clone()),
            );
            mark_sharp_file(&mut converted, &self.configuration.source.workspace);
            database.add(converted);
        }

        Ok(Session::new(orchestrator, database))
    }

    /// Proves `set`, indexes of `candidates`, with the ones `accepted` already holds, and adds each that keeps the
    /// project as it was to `accepted`. One proof runs for them all. A file with issues of its own stays plain PHP, and
    /// the rest are proven again, each file whose own issues may come from one that stays among them. An issue in
    /// another file sends the set to be proven in two parts, each on top of what is accepted, until one file is left to
    /// keep plain PHP. The second part is the second half of the fewest files one issue can come from, as
    /// [`Session::reached`] finds them, and the first part is the rest of the set. When no file of the set can bring an
    /// issue, the parts are the set's halves. When the first part is accepted whole and the second is one file, the
    /// proof that split them was that file on top of what is accepted, so its issues keep the file plain PHP with no
    /// proof more. Every proof is a whole analysis with fresh hosts, so each split costs a proof, and each stops its
    /// hosts before the parts start theirs. A proof that can't finish, as when a host's hook makes no progress, logs its
    /// error once and keeps the files of its set plain PHP, each naming the proof.
    fn prove(
        &self,
        plain: &Proof,
        candidates: &[Candidate],
        accepted: &mut Vec<usize>,
        mut set: Vec<usize>,
        outcome: &mut Outcome,
    ) {
        while !set.is_empty() {
            let swaps: Vec<&Candidate> = accepted.iter().chain(&set).map(|&index| &candidates[index]).collect();
            let start = Instant::now();
            outcome.proofs += 1;
            let proven = self.open(&swaps).and_then(|mut session| session.proof(self).map(|proof| (session, proof)));
            let (session, proof) = match proven {
                Ok(proven) => proven,
                Err(error) => {
                    // The error once, as a host's carries up to 64 KiB of its stderr, and each file names the proof.
                    let proof = outcome.proofs;
                    outcome.failed = true;
                    tracing::error!("Proof {proof} could not finish, so the files it proves stay plain PHP: {error}");
                    for &index in &set {
                        let name = &candidates[index].name;
                        let reason = format!("{}: proof {proof} could not finish", display(name));
                        outcome.refused.entry(name.clone()).or_default().push(reason);
                    }
                    return;
                }
            };
            let Judgement { own, mut others, expected } = judge(plain, &proof, &swaps);
            drop(proof);
            tracing::debug!(
                "Proof {} of {} files on {} accepted ran in {:?}: {} files with issues of their own, {} issues in others.",
                outcome.proofs,
                set.len(),
                accepted.len(),
                start.elapsed(),
                own.len(),
                others.len(),
            );
            let mut refused = Vec::new();
            for (name, found) in own {
                match set.iter().position(|&index| candidates[index].name == name) {
                    Some(position) => refused.push((set[position], found)),
                    // A file proven before gains an issue only with the files this proof adds.
                    None => others.extend(found),
                }
            }
            // A file's own issue may come from another file this proof keeps plain PHP, as a caller's does from the
            // class it passes a lambda to, so such a file is proven again without it. When every one may, each
            // stays plain PHP.
            let indexes: Vec<usize> = refused.iter().map(|(index, _)| *index).collect();
            let comes_from_another = |index: usize, found: &[Found]| {
                let rest: Vec<usize> = indexes.iter().copied().filter(|&other| other != index).collect();
                session.reached(found, candidates, &rest).iter().any(|reached| !reached.is_empty())
            };
            let again: Vec<bool> = refused.iter().map(|(index, found)| comes_from_another(*index, found)).collect();
            let all_again = again.iter().all(|&again| again);
            for ((index, found), again) in refused.into_iter().zip(again) {
                if again && !all_again {
                    continue;
                }
                set.retain(|&kept| kept != index);
                let name = candidates[index].name.clone();
                outcome.refused.entry(name).or_default().extend(found.into_iter().map(|found| found.shown));
            }
            if !indexes.is_empty() {
                continue;
            }
            if others.is_empty() {
                accepted.append(&mut set);
                outcome.expected = expected;
                return;
            }
            if let [index] = set[..] {
                outcome.adds(&candidates[index].name, others);
                return;
            }

            let narrowest = session
                .reached(&others, candidates, &set)
                .into_iter()
                .filter(|reached| !reached.is_empty())
                .min_by_key(Vec::len);
            // Each part's proof starts hosts of its own, so this proof's hosts and analysis stop first, and only the
            // issues that can keep the second part's one file plain stay.
            drop(session);
            drop(expected);
            let second = match narrowest {
                Some(mut suspects) => {
                    let second = suspects.split_off(suspects.len() / 2);
                    set.retain(|index| !second.contains(index));
                    second
                }
                None => set.split_off(set.len() / 2),
            };
            let (first, before) = (set.len(), accepted.len());
            self.prove(plain, candidates, accepted, set, outcome);
            // With the first part accepted whole, this proof was the one file of the second on top of what is
            // accepted, so its issues keep that file plain PHP with no proof more.
            if let [index] = second[..]
                && accepted.len() == before + first
            {
                outcome.adds(&candidates[index].name, others);
                return;
            }
            return self.prove(plain, candidates, accepted, second, outcome);
        }
    }

    /// Whether `written`, the converted files a run wrote, keep the project as `plain` found it with hosts that load
    /// them. The extension hosts load the project's own classes, so once their compile takes the written files in,
    /// fresh hosts prove the project again (spec section 27). A compile or proof that fails, and each issue the written
    /// files add, is logged as an error, as git holds their plain PHP.
    fn prove_written(&self, plain: &Proof, written: &[&Candidate]) -> bool {
        match compile_for_hosts(self.configuration) {
            Ok((_, HostsChange::Unchanged)) => return true,
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    "The converted files could not be compiled for the extension hosts, so no hosts that load them proved the project, and git holds their plain PHP: {error}"
                );
                return false;
            }
        }
        tracing::info!("The extension hosts load the converted files, so fresh hosts prove the project again.");
        let judgement = match self.open(&[]).and_then(|mut session| session.proof(self)) {
            Ok(proof) => judge(plain, &proof, written),
            Err(error) => {
                tracing::error!(
                    "Fresh hosts that load the converted files could not prove the project, and git holds its plain PHP: {error}"
                );
                return false;
            }
        };
        for (name, found) in &judgement.own {
            let name = display(&sharp_name(name));
            tracing::error!("{name} adds issues once the extension hosts load it, and git holds its plain PHP:");
            for found in found {
                tracing::error!("  {}", found.written);
            }
        }
        for found in &judgement.others {
            tracing::error!("The converted files add an issue with the hosts that load them: {}", found.written);
        }

        judgement.own.is_empty() && judgement.others.is_empty()
    }
}

impl<'configuration> Session<'configuration> {
    fn new(orchestrator: Orchestrator<'configuration>, database: Database<'_>) -> Self {
        let server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator));

        Self { _orchestrator: orchestrator, server }
    }

    /// Analyzes the project and compiles it in memory, with what `ignore` entries and the baseline hide of a host
    /// rule's issues hidden, as `mago analyze` and `mago compile` hide it. A converted file states no law, so there is
    /// none to prove.
    fn proof(&mut self, prover: &Prover<'_>) -> Result<Proof, Error> {
        let start = Instant::now();
        let analysis = self.server.analyze()?;
        let database = self.server.database().read_only();
        let issues = keys(&self.server, prover.hidden.shown(analysis.issues, &database));
        let ((compilations, _), _) = self.server.compile(
            Folder { last: &CompiledFiles::default(), compiled: &mut |_| Ok(None), intact: &mut |_| false },
            None,
            |path| unit::stamp(&prover.root, path),
            |_, _| Ok(Default::default()),
            &prover.standard_library,
            |issues| prover.hidden.shown(issues, &database),
        )?;
        let refused = compilations
            .into_iter()
            .filter_map(|(file_id, compilation)| match compilation {
                Compilation::Refused(errors) => database.get(&file_id).ok().map(|file| (file.name.to_vec(), errors)),
                Compilation::Accepted(_) | Compilation::Kept => None,
            })
            .collect();
        tracing::trace!("The project's proof ran in {:?}.", start.elapsed());

        Ok(Proof { issues, refused })
    }

    /// For each of `found`, issues in files outside `set`, the files of `set`, as indexes of `candidates`, it comes
    /// from: each whose converted file the issue points into, as an override's issue points at the method it
    /// overrides, or when it points into none, each whose edit makes the analysis's warm path re-analyze a file the
    /// issue points into, as an edit of a parent does a plain class that extends it, or an edit of a callee its caller.
    fn reached(&self, found: &[Found], candidates: &[Candidate], set: &[usize]) -> Vec<Vec<usize>> {
        let files: Vec<FileId> = found
            .iter()
            .flat_map(|found| &found.files)
            .map(|name| FileId::new(name))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let reaching: HashMap<FileId, _> = files.iter().copied().zip(self.server.files_reaching(&files)).collect();
        let converted: Vec<(usize, FileId)> =
            set.iter().map(|&index| (index, FileId::new(&sharp_name(&candidates[index].name)))).collect();

        found
            .iter()
            .map(|found| {
                let files: Vec<FileId> = found.files.iter().map(|name| FileId::new(name)).collect();
                let pointed: Vec<usize> = converted
                    .iter()
                    .filter(|(_, converted)| files.contains(converted))
                    .map(|(index, _)| *index)
                    .collect();
                if !pointed.is_empty() {
                    return pointed;
                }

                converted
                    .iter()
                    .filter(|(_, converted)| {
                        files.iter().any(|file| reaching.get(file).is_some_and(|files| files.contains(converted)))
                    })
                    .map(|(index, _)| *index)
                    .collect()
            })
            .collect()
    }

    /// The PHP# text of `file` with the `extern namespace` and `extern` lines it needs, or each construct that keeps it
    /// plain PHP, by its place. A built-in's call becomes the member of `library` that runs it, and an invoker lists the
    /// `effects` of the method it calls.
    fn convert(&self, file: &File, library: &Library, effects: &Effects) -> Result<Converted, Vec<String>> {
        let arena = LocalArena::new();
        let program = parse_file(&arena, file);
        let names = NameResolver::new(&arena).resolve(program);
        let Some((_, artifacts)) = self.server.analyze_file_with_artifacts(file.id) else {
            return Err(vec![format!("{}: the analysis holds no such file", display(&file.name))]);
        };
        let input = Input {
            file,
            program,
            names: &names,
            codebase: self.server.codebase(),
            artifacts: &artifacts,
            effects,
            references: self.server.symbol_references(),
            library,
        };

        convert(&input).map_err(|refusals| refusals.iter().map(|refusal| place(file, refusal)).collect())
    }
}

/// A fresh orchestrator with the project's extension hosts started, as `mago analyze` starts them, and the project's
/// database as `mago compile` builds it, the standard library's and each package's PHP# files included, so the proof
/// lowers each converted file with the library's inline forms, as `mago compile` does.
fn load(
    configuration: &Configuration,
    color_choice: ColorChoice,
) -> Result<(Orchestrator<'_>, Database<'static>), Error> {
    let orchestrator = whole_workspace(configuration, color_choice, false)?;
    if let Some(external_analyzer) = start_external_analyzer(
        &configuration.extension_hosts,
        configuration.php_version,
        &configuration.analyzer.plugins,
        configuration.analyzer.disable_default_plugins,
    ) {
        orchestrator.set_external_analyzer_handle(external_analyzer);
    }
    let (database, _) = project_database(&orchestrator, &configuration.source.workspace)?;

    Ok((orchestrator, database))
}

/// The text of each lowering of `proof` that panicked, in file name order, each a defect of the checker.
fn panics(proof: &Proof) -> Vec<String> {
    let mut refused: Vec<(&Vec<u8>, &IssueCollection)> = proof.refused.iter().collect();
    refused.sort_by(|a, b| a.0.cmp(b.0));

    refused
        .into_iter()
        .flat_map(|(_, errors)| errors.iter())
        .filter(|error| error.code.as_deref() == Some(LOWERING_PANIC))
        .map(|error| error.message.clone())
        .collect()
}

/// What `proof`, of the project with `swaps` converted, found that `plain`, of the plain project, didn't: each issue
/// by its file, level, code, message and line, with duplicates counted, and each file `mago compile` refuses. Every
/// issue of a converted file counts but a note, and decision 9's warnings are expected wherever they appear. An issue
/// of a converted file names its plain file, the one a dry run leaves as it is, at the member it is in, as
/// [`plain_place`] finds it.
fn judge(plain: &Proof, proof: &Proof, swaps: &[&Candidate]) -> Judgement {
    let owner = |file: &[u8]| swaps.iter().find(|candidate| sharp_name(&candidate.name) == file);
    let mut judgement = Judgement::default();
    for (key, (count, files, members)) in &proof.issues {
        let (file, level, code, message, line) = key;
        let before = plain.issues.get(key).map_or(0, |(count, ..)| *count);
        if count.saturating_sub(before) == 0 || *level < Level::Warning {
            continue;
        }
        let code = code.clone().unwrap_or_default();
        let owner = owner(file);
        let place = format!("{}:{line}", display(file));
        let (shown, written) = match (owner, members.first()) {
            (Some(candidate), Some(member)) => {
                (plain_place(candidate, members, *line), format!("{place}, in {member}"))
            }
            (Some(candidate), None) => (plain_place(candidate, members, *line), place),
            (None, _) => (place.clone(), place),
        };
        let found = Found {
            shown: format!("{shown}: {level} {code}: {message}"),
            written: format!("{written}: {level} {code}: {message}"),
            files: files.clone(),
        };
        if *level == Level::Warning && EXPECTED_WARNINGS.contains(&code.as_str()) {
            judgement.expected.push(found);
        } else if let Some(candidate) = owner {
            judgement.own.entry(candidate.name.clone()).or_default().push(found);
        } else {
            judgement.others.push(found);
        }
    }
    for (file, errors) in proof.refused.iter().filter(|(file, _)| !plain.refused.contains_key(*file)) {
        // The compile's first error says why it refuses the file. A lowering that panicked names a defect of the
        // checker, so its text is the reason whatever else the file holds, never hidden.
        let reason = errors
            .iter()
            .find(|error| error.code.as_deref() == Some(LOWERING_PANIC))
            .or_else(|| errors.iter().next())
            .map_or_else(String::new, |error| format!(": {}", error.message));
        let written = format!("{}: mago compile would refuse it{reason}", display(file));
        let found = |shown: String| Found { shown, written: written.clone(), files: BTreeSet::from([file.clone()]) };
        match owner(file) {
            Some(candidate) => judgement.own.entry(candidate.name.clone()).or_default().push(found(format!(
                "{}: mago compile would refuse it once converted{reason}",
                display(&candidate.name)
            ))),
            None => judgement.others.push(found(written.clone())),
        }
    }

    judgement
}

impl Outcome {
    /// Keeps the plain PHP file `name` plain for `others`, the issues converting it adds to other files.
    fn adds(&mut self, name: &[u8], others: Vec<Found>) {
        let reasons = others.into_iter().map(|found| format!("{}, which converting it adds", found.shown));
        self.refused.entry(name.to_vec()).or_default().extend(reasons);
    }

    /// Failure when the run itself failed or a path the command line names reaches no file, and success when the run
    /// completed, whatever files it kept plain PHP, since each keeps its reason in the report.
    fn exit_code(&self) -> ExitCode {
        if self.failed || !self.unreached.is_empty() { ExitCode::FAILURE } else { ExitCode::SUCCESS }
    }

    fn report(&self, dry_run: bool, color_choice: ColorChoice) {
        for reason in &self.unreached {
            tracing::error!("{reason}");
        }
        for (name, reasons) in &self.refused {
            let name = display(name);
            tracing::warn!("{name} stays plain PHP:");
            for reason in reasons {
                tracing::warn!("  {reason}");
            }
        }
        let mut formatter = PatchFormatter::new();
        if should_use_colors(color_choice) {
            formatter = formatter.with_color();
        }
        for (name, original, text) in &self.converted {
            println!("{} -> {}", display(name), display(&sharp_name(name)));
            if dry_run {
                let (original, text) = (String::from_utf8_lossy(original), String::from_utf8_lossy(text));
                println!("{}", formatter.fmt_patch(&diffy::create_patch(&original, &text)));
            }
        }
        if let Some((name, before, after)) = &self.externs
            && !self.converted.is_empty()
        {
            println!("{} holds the extern lines of the converted files", display(name));
            if dry_run {
                let (before, after) = (String::from_utf8_lossy(before), String::from_utf8_lossy(after));
                println!("{}", formatter.fmt_patch(&diffy::create_patch(&before, &after)));
            }
        }
        for warning in &self.expected {
            let warning = if dry_run { &warning.shown } else { &warning.written };
            tracing::info!("Expected after converting, decision 9: {warning}");
        }

        let converted = self.converted.len();
        let refused = self.refused.len();
        tracing::info!(
            "{} {converted} {} to PHP#, and {refused} {} plain PHP.",
            if dry_run { "Would convert" } else { "Converted" },
            if converted == 1 { "file" } else { "files" },
            if refused == 1 { "file stays" } else { "files stay" },
        );
        if converted > 0 {
            tracing::info!(
                "mago lint, mago guard and mago fmt read only plain PHP, so their rules no longer check the converted files."
            );
            tracing::info!(
                "PHPDoc types are now checked while the code runs, so run the project's tests on the PHP# engine and compare them test by test."
            );
        }
    }
}

/// Writes each text of `files` at the `.sharp` name of its committed plain PHP file, in order, until a write fails,
/// then records the renames of the files it wrote in git's index in one call, as `git mv` would, and only then removes
/// their plain files, so a write that fails leaves its plain file and those after it where they were. Returns how many
/// files it converted, and the error that stopped it. When git can't record the renames, it removes what it wrote, so
/// every file stays plain PHP.
fn write(workspace: &Path, files: &[(&[u8], &[u8])]) -> (usize, Option<Error>) {
    let mut renames = Vec::with_capacity(files.len());
    let mut stopped = None;
    for (name, text) in files {
        let (source, target) = (PathBuf::from(display(name)), PathBuf::from(display(&sharp_name(name))));
        if let Err(error) = write_by_rename(&workspace.join(&target), text) {
            stopped = Some(error.into());
            break;
        }
        renames.push((source, target));
    }
    if renames.is_empty() {
        return (0, stopped);
    }
    if let Err(error) = git::stage_renames(workspace, &renames) {
        for (_, target) in &renames {
            let target = workspace.join(target);
            if let Err(removing) = std::fs::remove_file(&target) {
                let target = target.display();
                tracing::error!("{target} could not be removed, beside the plain file it converts: {removing}");
            }
        }
        return (0, Some(error));
    }
    for (converted, (source, _)) in renames.iter().enumerate() {
        let source = workspace.join(source);
        if let Err(error) = std::fs::remove_file(&source) {
            return (converted, Some(DatabaseError::io(IOAction::DeleteFile, &source, error).into()));
        }
    }

    (renames.len(), stopped)
}

/// Each issue of `issues` by its file, level, code, message and line, with how many times it is reported, the files
/// its annotations point into, and the members of a PHP# file it is in.
fn keys(server: &Server, issues: IssueCollection) -> Issues {
    let mut keys = Issues::default();
    let mut declared: HashMap<FileId, Vec<(u32, u32, String)>> = HashMap::new();
    for issue in issues.iter() {
        let (file, line, within) = issue
            .primary_span()
            .and_then(|span| {
                let file = server.database().get(&span.file_id).ok()?;
                let offset = span.start.offset;
                let within: Vec<String> = if Dialect::of(&file).is_sharp() {
                    let members =
                        declared.entry(span.file_id).or_insert_with(|| members_of(server.codebase(), span.file_id));
                    members
                        .iter()
                        .filter(|(start, end, _)| (*start..*end).contains(&offset))
                        .map(|(_, _, member)| member.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                Some((file.name.to_vec(), file.line_number(offset) + 1, within))
            })
            .unwrap_or_default();
        let (count, files, members) =
            keys.entry((file, issue.level, issue.code.clone(), issue.message.clone(), line)).or_default();
        *count += 1;
        *members = within;
        files.extend(
            issue
                .annotations
                .iter()
                .filter_map(|annotation| server.database().get(&annotation.span.file_id).ok())
                .map(|file| file.name.to_vec()),
        );
    }

    keys
}

/// Whether `file` is one the converter takes: one class-like, named like the file, under a PSR-4 folder of
/// `composer.json`'s `autoload` (spec section 27). The reason it is not, otherwise.
fn shape(file: &File, roots: &[(String, PathBuf)]) -> Result<(), String> {
    let name = display(&file.name);
    if !roots.iter().any(|(_, folder)| Path::new(&name).starts_with(folder)) {
        return Err("it is under no PSR-4 folder of composer.json's autoload, so it is a script or a test".to_owned());
    }

    let arena = LocalArena::new();
    let program = parse_file(&arena, file);
    let declared: Vec<&[u8]> = statements(program)
        .into_iter()
        .filter_map(|statement| match statement {
            Statement::Class(class) => Some(class.name.value),
            Statement::Interface(interface) => Some(interface.name.value),
            Statement::Trait(r#trait) => Some(r#trait.name.value),
            Statement::Enum(r#enum) => Some(r#enum.name.value),
            _ => None,
        })
        .collect();
    let stem = Path::new(&name).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    match declared.as_slice() {
        [] => Err("it declares no class, interface or enum, so it is a script".to_owned()),
        [only] if *only == stem.as_bytes() => Ok(()),
        [only] => Err(format!(
            "it declares `{}`, and a .sharp file declares the type it is named after (spec section 27)",
            String::from_utf8_lossy(only)
        )),
        several => {
            Err(format!("it declares {} types, and a .sharp file declares one (spec section 27)", several.len()))
        }
    }
}

/// The statements of `program`, with those of each namespace in its place.
fn statements<'program>(program: &'program Program<'_>) -> Vec<&'program Statement<'program>> {
    let mut statements = Vec::new();
    for statement in &program.statements {
        match statement {
            Statement::Namespace(namespace) => statements.extend(namespace.statements().iter()),
            statement => statements.push(statement),
        }
    }

    statements
}

/// Each class-like the analysis declares in `file`, and each member of one, by its start and end and as plain PHP names
/// it: `Order`, `Order::total()`, `Order::$items` or `Order::LIMIT`. The shortest come first, so the members that hold
/// an offset are the innermost first, and a class-like comes after a member as long as it.
fn members_of(codebase: &CodebaseMetadata, file: FileId) -> Vec<(u32, u32, String)> {
    let mut members = Vec::new();
    for class in codebase.class_likes.values().filter(|class| class.span.file_id == file) {
        let name = class.original_name.as_str_lossy();
        let class_name = name.rsplit('\\').next().unwrap_or(&name).to_owned();
        for method in &class.methods {
            if let Some(method) = codebase.get_method(class.name, *method)
                && method.span.file_id == file
            {
                let named = format!("{class_name}::{}()", method.original_name.as_str_lossy());
                members.push((method.span.start.offset, method.span.end.offset, named));
            }
        }
        for (property, metadata) in &class.properties {
            if let Some(span) = metadata.span.filter(|span| span.file_id == file) {
                members.push((
                    span.start.offset,
                    span.end.offset,
                    format!("{class_name}::{}", property.as_str_lossy()),
                ));
            }
        }
        let constants = class.constants.iter().map(|(constant, metadata)| (constant, metadata.span));
        for (constant, span) in constants.chain(class.enum_cases.iter().map(|(case, metadata)| (case, metadata.span))) {
            if span.file_id == file {
                members.push((
                    span.start.offset,
                    span.end.offset,
                    format!("{class_name}::{}", constant.as_str_lossy()),
                ));
            }
        }
        members.push((class.span.start.offset, class.span.end.offset, class_name));
    }
    members.sort_by_key(|(start, end, _)| end - start);

    members
}

/// Where an issue at `line` of `candidate`'s converted file is in its plain PHP file, the one the user has: the file,
/// the line and name of the innermost of `members`, as [`members_of`] names them, that the plain file declares, and
/// `line`, which a dry run's diff shows. The plain line within the member is not known, since the converter gives its
/// text and not where each part of it came from.
fn plain_place(candidate: &Candidate, members: &[String], line: u32) -> String {
    let name = display(&candidate.name);
    let file = File::ephemeral(Cow::Owned(candidate.name.clone()), Cow::Owned(candidate.original.clone()));
    let arena = LocalArena::new();
    let program = parse_file(&arena, &file);
    let line_of = |offset: u32| file.line_number(offset) + 1;
    let lossy = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let mut declared: Vec<(String, u32)> = Vec::new();
    for statement in statements(program) {
        let (class, class_members) = match statement {
            Statement::Class(class) => (&class.name, &class.members),
            Statement::Interface(interface) => (&interface.name, &interface.members),
            Statement::Enum(r#enum) => (&r#enum.name, &r#enum.members),
            _ => continue,
        };
        let class_name = lossy(class.value);
        declared.push((class_name.clone(), line_of(class.span.start.offset)));
        for member in class_members.iter() {
            match member {
                ClassLikeMember::Method(method) => {
                    let named = format!("{class_name}::{}()", lossy(method.name.value));
                    declared.push((named, line_of(method.name.span.start.offset)));
                    for parameter in method.parameter_list.parameters.iter() {
                        if parameter.is_promoted_property() {
                            let named = format!("{class_name}::{}", lossy(parameter.variable.name));
                            declared.push((named, line_of(parameter.variable.span.start.offset)));
                        }
                    }
                }
                ClassLikeMember::Property(property) => {
                    for variable in property.variables() {
                        declared.push((
                            format!("{class_name}::{}", lossy(variable.name)),
                            line_of(variable.span.start.offset),
                        ));
                    }
                }
                ClassLikeMember::Constant(constant) => {
                    for item in constant.items.iter() {
                        declared.push((
                            format!("{class_name}::{}", lossy(item.name.value)),
                            line_of(item.name.span.start.offset),
                        ));
                    }
                }
                ClassLikeMember::EnumCase(case) => {
                    let item = case.item.name();
                    declared.push((format!("{class_name}::{}", lossy(item.value)), line_of(item.span.start.offset)));
                }
                _ => {}
            }
        }
    }

    match members.iter().find_map(|member| declared.iter().find(|(declared, _)| declared.eq_ignore_ascii_case(member)))
    {
        Some((member, declared)) => format!("{name}:{declared}, in {member}, at line {line} of its PHP#"),
        None => format!("{name}, at line {line} of its PHP#"),
    }
}

/// Why `path`, a path the command line names, relative to the workspace at `root` unless it is outside it, reaches no
/// plain PHP file the project's analysis loads.
fn unreached(configuration: &Configuration, root: &Path, path: &Path) -> Result<String, Error> {
    let (shown, on_disk) = (path.display(), root.join(path));
    if !on_disk.exists() {
        return Ok(format!("{shown} doesn't exist."));
    }
    if path.is_absolute() {
        return Ok(format!("{shown} is outside the workspace {}.", root.display()));
    }
    let sources = &configuration.source.paths;
    let inside = sources.is_empty()
        || if on_disk.is_dir() {
            // A folder holds the files of a source path when it is in the source path's folder, the part before its
            // first glob, as the loader walks it, or holds that folder.
            sources.iter().any(|source| {
                let folder: PathBuf = Path::new(source)
                    .components()
                    .filter(|part| !matches!(part, Component::CurDir))
                    .take_while(|part| !contains_glob_metacharacters(part.as_os_str().to_string_lossy()))
                    .collect();
                path.starts_with(&folder) || folder.starts_with(path)
            })
        } else {
            let matcher = ExclusionMatcher::compile(
                sources.iter().map(String::as_str),
                configuration.source.glob.to_database_settings(),
            )?;
            matcher.is_match(&path.to_string_lossy())
        };

    Ok(if !inside {
        format!(
            "{shown} is outside the source paths ({}), so the project's analysis doesn't load it.",
            sources.join(", ")
        )
    } else if on_disk.is_dir() {
        format!("{shown} holds no plain PHP file the project's analysis loads.")
    } else if path.extension().is_some_and(|extension| extension == "php") {
        format!("{shown} is excluded from the project's analysis.")
    } else {
        format!("{shown} is no plain PHP file.")
    })
}

/// Each PSR-4 folder of `composer.json`'s `autoload`, relative to the workspace, with its namespace, as `App` for
/// `App\`, in the order `composer.json` lists them. A test under `autoload-dev` stays out, since a test runner loads a
/// test only from the file it lists (spec section 27).
fn psr4_roots(workspace: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let path = workspace.join("composer.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let composer: ComposerPackage = contents.parse()?;
    let mut roots = Vec::new();
    for (namespace, value) in composer.autoload.map(|autoload| autoload.psr_4).unwrap_or_default() {
        let folders = match value {
            AutoloadPsr4value::String(folder) => vec![folder],
            AutoloadPsr4value::Array(folders) => folders,
        };
        let namespace = namespace.trim_matches('\\');
        roots.extend(folders.iter().map(|folder| (namespace.to_owned(), PathBuf::from(folder.trim_end_matches('/')))));
    }

    Ok(roots)
}

impl Externs {
    /// The declarations file of the PSR-4 folder `root` of the project at `workspace`, with its namespace, and the
    /// text it holds now, which is none when it is missing.
    fn new(workspace: &Path, (namespace, folder): &(String, PathBuf)) -> Result<Self, Error> {
        let name = folder.join("Stubs").join("Externs.sharp").to_string_lossy().into_owned().into_bytes();
        let path = workspace.join(display(&name));
        let held = match std::fs::read(&path) {
            Ok(held) => held,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &path, error).into()),
        };

        Ok(Self { name, path, namespace: format!("{}.Stubs", namespace.replace('\\', ".")), held })
    }

    /// The text the file holds now, and its text with each line `candidates` need that it lacks, or `None` when it
    /// holds every one. A new file declares its namespace. Each `import`, `extern namespace` and `extern` line goes
    /// among the lines of its kind, before the first that sorts after it, or after the last one, so lines that are
    /// sorted stay sorted. A class or a namespace whose short name another import takes is imported under its
    /// namespace's last parts before it, as `import Vendor.Clock as VendorClock;`. A class one of the candidates
    /// declares takes no line.
    fn text(&self, candidates: &[&Candidate]) -> Option<(Vec<u8>, Vec<u8>)> {
        let held = String::from_utf8_lossy(&self.held);
        let mut lines: Vec<String> = if held.trim().is_empty() {
            vec![format!("namespace {};", self.namespace)]
        } else {
            held.lines().map(str::to_owned).collect()
        };
        let mut added = false;
        let namespaces: BTreeSet<&str> =
            candidates.iter().flat_map(|candidate| &candidate.extern_namespaces).map(String::as_str).collect();
        for line in namespaces {
            added |= put_line(&mut lines, Line::ExternNamespace, line.to_owned());
        }
        // A class that converts with the candidates is PHP#, which takes no line, so a line an earlier run wrote for it
        // goes, with its import.
        let converts = |class: &str| {
            candidates
                .iter()
                .flat_map(|candidate| &candidate.declares)
                .any(|declared| declared.eq_ignore_ascii_case(class))
        };
        let gone: Vec<(String, String)> = lines
            .iter()
            .filter_map(|line| {
                let import = line.trim().strip_prefix("import ")?.strip_suffix(';')?;
                let (path, alias) = import.split_once(" as ").unwrap_or((import, import.rsplit('.').next()?));
                converts(&path.replace('.', "\\")).then(|| (line.trim().to_owned(), alias.to_owned()))
            })
            .collect();
        if !gone.is_empty() {
            let names = |line: &str, alias: &str| {
                line.strip_prefix("extern ")
                    .and_then(|target| target.strip_prefix(alias))
                    .is_some_and(|rest| rest.starts_with([' ', '.', ';']))
            };
            lines.retain(|line| {
                let held = line.trim();
                !gone.iter().any(|(import, alias)| held == import || names(held, alias))
            });
            lines.dedup_by(|next, blank| next.trim().is_empty() && blank.trim().is_empty());
            while lines.last().is_some_and(|line| line.trim().is_empty()) {
                lines.pop();
            }
            added = true;
        }
        let undeclared: BTreeSet<&Undeclared> = candidates
            .iter()
            .flat_map(|candidate| &candidate.undeclared)
            .filter(|undeclared| !matches!(undeclared, Undeclared::Class(class) if converts(class)))
            .collect();
        for undeclared in undeclared {
            let line = match undeclared {
                Undeclared::Class(class) => format!("extern {} uses any;", imported(&mut lines, class, &mut added)),
                Undeclared::Function(namespace, function) => {
                    format!("extern {}.{function} uses any;", imported(&mut lines, namespace, &mut added))
                }
            };
            added |= put_line(&mut lines, Line::Extern, line);
        }

        added.then(|| (self.held.clone(), format!("{}\n", lines.join("\n")).into_bytes()))
    }
}

/// The kinds of line a declarations file holds, in the order they stand in it.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Line {
    Namespace,
    Import,
    ExternNamespace,
    Extern,
}

impl Line {
    fn of(line: &str) -> Option<Self> {
        let line = line.trim_start();
        if line.starts_with("namespace ") {
            Some(Self::Namespace)
        } else if line.starts_with("import ") {
            Some(Self::Import)
        } else if line.starts_with("extern namespace ") {
            Some(Self::ExternNamespace)
        } else {
            line.starts_with("extern ").then_some(Self::Extern)
        }
    }
}

/// Puts `line`, of the kind `kind`, in `lines` unless it is there: before the first line of its kind that sorts after
/// it, or after the last one, or, as the first of its kind, after the lines of the kinds before it, past a blank line.
/// Whether it was put.
fn put_line(lines: &mut Vec<String>, kind: Line, line: String) -> bool {
    if lines.iter().any(|held| held.trim() == line) {
        return false;
    }
    let of_kind = |held: &String| Line::of(held) == Some(kind);
    let at = match lines.iter().position(|held| of_kind(held) && held.trim() > line.as_str()) {
        Some(at) => at,
        None => match lines.iter().rposition(of_kind) {
            Some(last) => last + 1,
            None => {
                let after = lines.iter().rposition(|held| Line::of(held).is_some_and(|held| held < kind));
                let at = after.map_or(lines.len(), |after| after + 1);
                lines.insert(at, String::new());
                at + 1
            }
        },
    };
    lines.insert(at, line);

    true
}

/// The name the file's lines write for `name`, a class or a namespace as PHP writes it: the alias an `import` line of
/// it gives, or the one a new `import` line gives, its last part, or its namespace's last parts before it when another
/// import or PHP# takes that, as `PaymentsError` for `Lib\Payments\Error`.
fn imported(lines: &mut Vec<String>, name: &str, added: &mut bool) -> String {
    let dotted = name.trim_start_matches('\\').replace('\\', ".");
    let imports: Vec<(String, String)> = lines
        .iter()
        .filter_map(|line| {
            let import = line.trim().strip_prefix("import ")?.strip_suffix(';')?;
            Some(match import.split_once(" as ") {
                Some((target, alias)) => (target.trim().to_owned(), alias.trim().to_owned()),
                None => (import.to_owned(), import.rsplit('.').next().unwrap_or(import).to_owned()),
            })
        })
        .collect();
    if let Some((_, alias)) = imports.iter().find(|(target, _)| target.eq_ignore_ascii_case(&dotted)) {
        return alias.clone();
    }
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut alias = parts[parts.len() - 1].to_owned();
    let mut prefix = parts.len() - 1;
    while prefix > 0
        && (is_reserved_alias(&alias) || imports.iter().any(|(_, taken)| taken.eq_ignore_ascii_case(&alias)))
    {
        prefix -= 1;
        alias = format!("{}{alias}", parts[prefix]);
    }
    let line = if alias == parts[parts.len() - 1] {
        format!("import {dotted};")
    } else {
        format!("import {dotted} as {alias};")
    };
    *added |= put_line(lines, Line::Import, line);

    alias
}

/// `path`, as the command line names it, relative to the workspace.
fn relative(workspace: &Path, path: &Path) -> PathBuf {
    let absolute =
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    let workspace = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());

    absolute.strip_prefix(&workspace).map(Path::to_path_buf).unwrap_or(absolute)
}

fn sharp_name(name: &[u8]) -> Vec<u8> {
    [name.strip_suffix(b".php").unwrap_or(name), b".sharp"].concat()
}

fn display(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

/// `file:line:column: reason` for a refusal in `file`.
fn place(file: &File, refusal: &Refusal) -> String {
    let line = file.line_number(refusal.span.start.offset) + 1;
    let column = file.column_number(refusal.span.start.offset) + 1;

    format!("{}:{line}:{column}: {}", display(&file.name), refusal.reason)
}

#[cfg(test)]
mod tests {
    use mago_reporting::Issue;

    use super::*;
    use crate::config::CliOverrides;

    /// An `ignore` entry hides an extension host rule's issue from the proof of a conversion, and never a language
    /// issue it names, which `mago compile` reports whatever the configuration says (spec section 27).
    #[test]
    fn the_configuration_hides_a_host_rule_issue_and_never_a_language_one() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("mago.toml");
        std::fs::write(&file, "php-version = \"8.5\"\n\n[analyzer]\nignore = [\"proof/marker\", \"mixed-argument\"]\n")
            .unwrap();
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root),
            config: Some(file),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();
        let issues = IssueCollection::from(vec![
            Issue::warning("This declaration is refined.").with_code("proof/marker"),
            Issue::error("The first value for `echo` is too general.").with_code("mixed-argument"),
        ]);

        let shown = Hidden::new(&configuration).unwrap().shown(issues, &ReadDatabase::empty());

        let codes: Vec<_> = shown.iter().map(|issue| issue.code.as_deref()).collect();
        assert_eq!(codes, [Some("mixed-argument")]);
    }

    /// A dry run of `src/Contracts` in a project of `files`, each a path under `src/` and its code, with `src/` the
    /// PSR-4 folder of `Generated\`.
    fn convert_contracts(files: &[(String, String)]) -> (ExitCode, Outcome) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = project(&root, files);

        ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap()
    }

    /// Commits every file under `root` to a fresh git repository, which holds the plain PHP a conversion replaces.
    fn commit_all(root: &Path) {
        let commit = ["-c", "user.name=test", "-c", "user.email=test@localhost", "commit", "--quiet", "-m", "plain"];
        for arguments in [&["init", "--quiet"][..], &["add", "--all"], &commit] {
            let output =
                mago_database::process::output(std::process::Command::new("git").args(arguments).current_dir(root))
                    .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
    }

    /// The configuration of a library at `root` of `files`, each a path under `src/` and its code, with `src/` the
    /// PSR-4 folder of `Generated\`, all committed to git. It is a library so that spec section 5's strict rules, which
    /// run on a project, stay out of the tests about something else.
    fn project(root: &Path, files: &[(String, String)]) -> Configuration {
        std::fs::write(root.join("mago.toml"), "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n").unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"type": "library", "autoload": {"psr-4": {"Generated\\": "src/"}}}"#,
        )
        .unwrap();
        for (path, code) in files {
            let path = root.join("src").join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, code).unwrap();
        }
        commit_all(root);

        Configuration::load(CliOverrides {
            workspace: Some(root.to_path_buf()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap()
    }

    /// An interface `Clean{index}` that converts with no issue anywhere.
    fn clean(index: usize) -> (String, String) {
        (
            format!("Contracts/Clean{index}.php"),
            format!(
                "<?php\n\nnamespace Generated\\Contracts;\n\ninterface Clean{index}\n{{\n    public function id(): int;\n}}\n"
            ),
        )
    }

    /// An interface `Sink{index}` that takes a `Map` once converted, and its plain PHP implementer `Drain{index}`, which
    /// no longer fits it.
    fn sink(index: usize) -> [(String, String); 2] {
        [
            (
                format!("Contracts/Sink{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Contracts;\n\ninterface Sink{index}\n{{\n    /** @param array<string, int> $items */\n    public function take(array $items): void;\n}}\n"
                ),
            ),
            (
                format!("Plain/Drain{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Plain;\n\nuse Generated\\Contracts\\Sink{index};\n\nfinal class Drain{index} implements Sink{index}\n{{\n    /** @param array<string, int> $items */\n    public function take(array $items): void\n    {{\n    }}\n}}\n"
                ),
            ),
        ]
    }

    /// An interface `Named{index}` whose `name()` inherits a `non-empty-string` from the PHPDoc of its plain PHP parent
    /// `Base{index}`, which PHP# writes `string`, and its plain PHP caller `Greeter{index}`, which passes the name where
    /// a `non-empty-string` goes. Once the interface converts, the caller gains an issue that points into no converted
    /// file. The parent's namespace is neither the interface's nor the caller's, so no two namespaces import each other.
    fn named(index: usize) -> [(String, String); 3] {
        [
            (
                format!("Bases/Base{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Bases;\n\ninterface Base{index}\n{{\n    /** @return non-empty-string */\n    public function name(): string;\n}}\n"
                ),
            ),
            (
                format!("Contracts/Named{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Contracts;\n\nuse Generated\\Bases\\Base{index};\n\ninterface Named{index} extends Base{index}\n{{\n    public function name(): string;\n}}\n"
                ),
            ),
            (
                format!("Plain/Greeter{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Plain;\n\nuse Generated\\Contracts\\Named{index};\n\nfinal class Greeter{index}\n{{\n    public function greet(Named{index} $named): string\n    {{\n        return $this->hello($named->name());\n    }}\n\n    /** @param non-empty-string $name */\n    private function hello(string $name): string\n    {{\n        return 'hello ' . $name;\n    }}\n}}\n"
                ),
            ),
        ]
    }

    /// One of thirteen interfaces gives its plain PHP caller an issue that points into no converted file. Only that
    /// interface's edit makes the analysis re-analyze the caller, so the first proof sets it apart, and the second
    /// accepts the other twelve: two proofs. Halving the whole set took eight.
    #[test]
    fn an_issue_that_points_into_no_converted_file_sets_apart_the_file_whose_edit_reaches_it() {
        let mut files: Vec<(String, String)> = (0..12).map(clean).collect();
        files.extend(named(0));

        let (exit_code, outcome) = convert_contracts(&files);

        // A file that stays plain PHP is an outcome the report names, never a failure of the run.
        assert_eq!(exit_code, ExitCode::SUCCESS);
        let refused: Vec<String> = outcome.refused.keys().map(|name| display(name)).collect();
        assert_eq!(refused, ["src/Contracts/Named0.php"], "{:#?}", outcome.refused);
        assert_eq!(outcome.converted.len(), 12);
        assert_eq!(outcome.proofs, 2);
    }

    /// Three of eight interfaces take a `Map` once converted, which their plain PHP implementers no longer fit, so each
    /// adds an issue in another file only that interface can bring. Each split sets one of them apart, so the five
    /// clean interfaces are proven together once, the last one set apart stays plain PHP for the issue the proof before
    /// found, and the other two are proven alone: six proofs, each a whole analysis of the project. Halving the set
    /// took eight.
    #[test]
    fn the_files_that_can_bring_an_issue_in_another_file_are_proven_apart_from_the_rest() {
        let mut files: Vec<(String, String)> = (0..5).map(clean).collect();
        files.extend((0..3).flat_map(sink));

        let (exit_code, outcome) = convert_contracts(&files);

        assert_eq!(exit_code, ExitCode::SUCCESS);
        let refused: Vec<String> = outcome.refused.keys().map(|name| display(name)).collect();
        assert_eq!(refused, ["src/Contracts/Sink0.php", "src/Contracts/Sink1.php", "src/Contracts/Sink2.php"]);
        assert!(
            outcome.refused.values().flatten().all(|reason| reason.ends_with(", which converting it adds")),
            "{:#?}",
            outcome.refused
        );
        assert_eq!(outcome.converted.len(), 5);
        assert_eq!(outcome.proofs, 6);
    }

    /// A caller passes a lambda that calls plain PHP to a class that stores it. Converted, that class stores a pure
    /// function, and its own `touch` passes it such a lambda too, an issue of its own the proof finds, so it stays plain
    /// PHP. The caller's own issue, that the stored function can't hold its lambda, comes from that class's conversion
    /// alone, so the caller is proven again without it and converts: two proofs. `Store::call` gives its callback's
    /// type, since the converter itself refuses a `mixed` returned as an unbounded type parameter, before any proof.
    #[test]
    fn a_file_whose_own_issue_comes_from_a_file_kept_plain_is_proven_again_without_it() {
        let files = [
            (
                "Plain/Store.php".to_owned(),
                "<?php\n\nnamespace Generated\\Plain;\n\nuse Closure;\n\nfinal class Store\n{\n    /**\n     * @template R\n     *\n     * @param (Closure(): R) $callback\n     *\n     * @return R\n     */\n    public static function call(Closure $callback): mixed\n    {\n        return $callback();\n    }\n\n    public static function touch(): void\n    {\n    }\n}\n".to_owned(),
            ),
            (
                "Contracts/Transaction.php".to_owned(),
                "<?php\n\nnamespace Generated\\Contracts;\n\nuse Closure;\nuse Generated\\Plain\\Store;\n\n/** @template T */\nfinal class Transaction\n{\n    /** @param (Closure(): T) $callback */\n    public function __construct(private Closure $callback)\n    {\n    }\n\n    public function touch(): void\n    {\n        (new self(function () {\n            Store::touch();\n        }))->run();\n    }\n\n    /** @return T */\n    public function run(): mixed\n    {\n        return Store::call($this->callback);\n    }\n}\n".to_owned(),
            ),
            (
                "Contracts/Permissions.php".to_owned(),
                "<?php\n\nnamespace Generated\\Contracts;\n\nuse Generated\\Plain\\Store;\n\nfinal class Permissions\n{\n    public function update(): void\n    {\n        (new Transaction(function () {\n            Store::touch();\n        }))->run();\n    }\n}\n".to_owned(),
            ),
        ];

        let (exit_code, outcome) = convert_contracts(&files);

        assert_eq!(exit_code, ExitCode::SUCCESS);
        let refused: Vec<String> = outcome.refused.keys().map(|name| display(name)).collect();
        assert_eq!(refused, ["src/Contracts/Transaction.php"], "{:#?}", outcome.refused);
        let converted: Vec<String> = outcome.converted.iter().map(|(name, ..)| display(name)).collect();
        assert_eq!(converted, ["src/Contracts/Permissions.php"]);
        assert_eq!(outcome.proofs, 2);
    }

    /// An extension host whose one plugin appends to the log `HOST_LOG` names: `start` and the worker's process id as
    /// an interpreter starts the worker, `exit` and the id as it ends, and `proof` and how many workers the log names
    /// alive as each analysis starts. Its after-analysis hooks never finish, and log `hang`, in the proof `HANGS_IN`
    /// names: `the second proof of one sink`, logging `one sink` and the sink of each analysis that converts one,
    /// or `the proof of the written files`, the first whose converted files are on disk.
    const HOST_COUNTING_ITS_WORKERS: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

$log = getenv('HOST_LOG');
$worker = random_int(1, 0xFFFFFFFF);
file_put_contents($log, "start {$worker}\n", FILE_APPEND | LOCK_EX);
register_shutdown_function(static fn() => file_put_contents($log, "exit {$worker}\n", FILE_APPEND | LOCK_EX));

final class CountsWorkers implements Plugin, BeforeAnalysisHook, AfterAnalysisHook
{
    public function __construct(
        private readonly string $log,
        private readonly ?string $hangsIn,
    ) {}

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('counts-workers', 'Counts workers', 'Logs the workers alive as each analysis starts.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
        $registry->registerAfterAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $alive = 0;
        foreach (file($this->log, FILE_IGNORE_NEW_LINES) as $line) {
            $alive += match (explode(' ', $line)[0]) {
                'start' => 1,
                'exit' => -1,
                default => 0,
            };
        }
        file_put_contents($this->log, "proof {$alive}\n", FILE_APPEND | LOCK_EX);
    }

    public function afterAnalysis(AfterAnalysisContext $context): void
    {
        $converted = [];
        foreach ($context->analysis->files as $file) {
            if (str_ends_with($file->file, '.sharp')) {
                $converted[] = $file->file;
            }
        }
        $hung = match ($this->hangsIn) {
            'the second proof of one sink' => $this->secondProofOfOneSink($converted),
            'the proof of the written files' => $converted !== [] && is_file($converted[0]) ? 'written' : null,
            default => null,
        };
        if ($hung === null) {
            return;
        }

        file_put_contents($this->log, "hang {$hung}\n", FILE_APPEND | LOCK_EX);
        while (true) {
            sleep(1);
        }
    }

    /**
     * Logs the sink of an analysis that converts one, and returns it in the second such analysis.
     *
     * @param list<string> $converted
     */
    private function secondProofOfOneSink(array $converted): ?string
    {
        $sinks = array_values(array_filter($converted, static fn(string $file): bool => str_contains($file, '/Sink')));
        if (count($sinks) !== 1) {
            return null;
        }

        $before = substr_count((string) file_get_contents($this->log), 'one sink ');
        file_put_contents($this->log, "one sink {$sinks[0]}\n", FILE_APPEND | LOCK_EX);

        return $before === 1 ? $sinks[0] : null;
    }
}

(new Worker(new Extension('counts-workers', 'Counts workers', '0.0.0', analyzerPlugins: [
    new CountsWorkers($log, getenv('HANGS_IN') ?: null),
])))->run();
"#;

    /// Whether the PHP SDK's Composer dependencies are installed, which `MAGO_REQUIRE_PHP_SDK_TESTS` requires.
    fn php_sdk() -> bool {
        let available = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/autoload.php").is_file();
        assert!(
            available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(),
            "vendor dependencies are required for the extension host tests"
        );
        available
    }

    /// The configuration of a project at `root` of `files`, as [`project`] builds it, with an extension host `counter` of
    /// [`HOST_COUNTING_ITS_WORKERS`] with a deadline of 15 s, which logs to `host.log`, and never finishes the
    /// after-analysis hooks of the proof `hangs_in` names. Each proof's hooks finish in under 1 s, so a loaded machine
    /// that stops one for seconds still lets it finish.
    fn hosted(root: &Path, files: &[(String, String)], hangs_in: Option<&str>) -> Configuration {
        project(root, files);
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(root.join("tools/host.php"), HOST_COUNTING_ITS_WORKERS).unwrap();
        let mut toml = std::fs::read_to_string(root.join("mago.toml")).unwrap();
        toml.push_str(&format!(
            "\n[extension-hosts.counter]\nfile = \"tools/host.php\"\nrequest-timeout-ms = 15000\nenvironment = {{ HOST_LOG = {:?}{} }}\n",
            root.join("host.log").display().to_string(),
            hangs_in.map(|proof| format!(", HANGS_IN = {proof:?}")).unwrap_or_default(),
        ));
        std::fs::write(root.join("mago.toml"), toml).unwrap();

        Configuration::load(CliOverrides {
            workspace: Some(root.to_path_buf()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap()
    }

    /// What `run` logs at `info` and above on this thread, after what it returns. While one dispatcher exists, tracing
    /// keeps a log line's interest from the dispatcher of the first thread to reach it, so a test running beside this
    /// one with none would silence the line here: a second dispatcher, alive for the whole run, makes tracing ask every
    /// one.
    fn logged<T>(run: impl FnOnce() -> T) -> (T, String) {
        static SECOND: std::sync::LazyLock<tracing::Dispatch> =
            std::sync::LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
        std::sync::LazyLock::force(&SECOND);
        let log = tempfile::NamedTempFile::new().unwrap();
        let path = log.path().to_path_buf();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || std::fs::OpenOptions::new().append(true).open(&path).unwrap())
            .finish();
        let value = tracing::subscriber::with_default(subscriber, run);

        (value, std::fs::read_to_string(log.path()).unwrap())
    }

    /// Each proof stops its extension hosts before the next proof starts its own, so a split keeps none of the hosts of
    /// the proofs it runs under. With one worker a host, one worker is alive as each analysis starts, where the six
    /// proofs of three sinks, each split kept with its hosts while it proved its parts, had 1, 2, 3, 4, 3 and 2.
    #[test]
    fn each_proof_stops_its_hosts_before_the_next_proof_starts_its_own() {
        if !php_sdk() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut files: Vec<(String, String)> = (0..5).map(clean).collect();
        files.extend((0..3).flat_map(sink));
        let configuration = hosted(&root, &files, None);

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        let log = std::fs::read_to_string(root.join("host.log")).unwrap();
        let alive: Vec<&str> = log.lines().filter_map(|line| line.strip_prefix("proof ")).collect();
        assert_eq!(outcome.proofs, 6);
        assert_eq!(alive, ["1"; 7], "{log}");
    }

    /// A proof that can't finish keeps the files it proves plain PHP with the error as their reason, and the run still
    /// reports what every other proof found. The three sinks split as in the test above, so the fifth proof is one sink
    /// on top of the five clean interfaces accepted, and the host's after-analysis hooks make no progress there.
    #[test]
    fn a_proof_that_cannot_finish_keeps_its_files_plain_php_and_the_run_reports_the_rest() {
        if !php_sdk() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let mut files: Vec<(String, String)> = (0..5).map(clean).collect();
        files.extend((0..3).flat_map(sink));
        let configuration = hosted(&root, &files, Some("the second proof of one sink"));

        let ((exit_code, outcome), logged) = logged(|| {
            ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
                .run(configuration, ColorChoice::Never)
                .unwrap()
        });

        let log = std::fs::read_to_string(root.join("host.log")).unwrap();
        let hung = log.lines().find_map(|line| line.strip_prefix("hang ")).expect(&log).replace(".sharp", ".php");
        assert_eq!(exit_code, ExitCode::FAILURE);
        assert_eq!(outcome.converted.len(), 5);
        assert_eq!(outcome.proofs, 6);
        let refused: Vec<String> = outcome.refused.keys().map(|name| display(name)).collect();
        assert_eq!(refused, ["src/Contracts/Sink0.php", "src/Contracts/Sink1.php", "src/Contracts/Sink2.php"]);
        for (name, reasons) in &outcome.refused {
            if *name == hung.as_bytes() {
                assert_eq!(reasons, &[format!("{hung}: proof 5 could not finish")]);
            } else {
                assert!(reasons.iter().all(|reason| reason.ends_with(", which converting it adds")), "{reasons:#?}");
            }
        }
        assert_eq!(logged.matches(TIMEOUT).count(), 1, "{logged}");
        assert!(
            logged.contains(&format!("Proof 5 could not finish, so the files it proves stay plain PHP: {TIMEOUT}")),
            "{logged}"
        );
    }

    /// The error the host of [`hosted`] gives when its after-analysis hooks make no progress.
    const TIMEOUT: &str = "Orchestrator error: Analysis error: Plugin Error: External plugin error: external analyzer worker failed: extension host `counter` made no progress for 15000 ms on the after-analysis hooks of plugin `counts-workers`: raise its `request-timeout-ms` for hooks that run that long";

    /// Once a run writes its files, fresh hosts that load them prove the project again. When that proof can't finish,
    /// the files are written and git holds their plain PHP, so the run still names them, logs the error, and fails as
    /// when those hosts find an issue.
    #[test]
    fn a_proof_of_the_written_files_that_cannot_finish_still_reports_them_and_fails() {
        if !php_sdk() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = hosted(&root, &[clean(0), clean(1)], Some("the proof of the written files"));

        let ((exit_code, outcome), logged) = logged(|| {
            ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: false }
                .run(configuration, ColorChoice::Never)
                .unwrap()
        });

        let log = std::fs::read_to_string(root.join("host.log")).unwrap();
        assert!(log.contains("hang written"), "{log}");
        assert_eq!(exit_code, ExitCode::FAILURE);
        assert!(outcome.refused.is_empty(), "{:#?}", outcome.refused);
        let converted: Vec<String> = outcome.converted.iter().map(|(name, _, _)| display(name)).collect();
        assert_eq!(converted, ["src/Contracts/Clean0.php", "src/Contracts/Clean1.php"]);
        assert!(root.join("src/Contracts/Clean1.sharp").exists() && !root.join("src/Contracts/Clean1.php").exists());
        assert!(
            logged.contains(&format!(
                "Fresh hosts that load the converted files could not prove the project, and git holds its plain PHP: {TIMEOUT}"
            )),
            "{logged}"
        );
    }

    /// Once a run writes its files, the project is compiled for the extension hosts so they load them. When that
    /// compile fails, here because the hosts' folder in Mago's cache is a file, no hosts prove the written files, so
    /// the run still names them, logs the error, and fails as when those hosts find an issue.
    #[test]
    fn a_compile_of_the_written_files_for_the_hosts_that_fails_still_reports_them_and_fails() {
        if !php_sdk() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = hosted(&root, &[clean(0), clean(1)], None);
        let project: String =
            unit::source_hash(root.as_os_str().as_encoded_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
        let folder = mago_sharp_lean::cache_root().unwrap().join("hosts").join(project);
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        std::fs::write(&folder, "").unwrap();

        let (ran, logged) = logged(|| {
            ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: false }
                .run(configuration, ColorChoice::Never)
        });
        std::fs::remove_file(&folder).unwrap();

        let (exit_code, outcome) = ran.unwrap();
        assert_eq!(exit_code, ExitCode::FAILURE);
        assert!(outcome.refused.is_empty(), "{:#?}", outcome.refused);
        let converted: Vec<String> = outcome.converted.iter().map(|(name, _, _)| display(name)).collect();
        assert_eq!(converted, ["src/Contracts/Clean0.php", "src/Contracts/Clean1.php"]);
        assert!(root.join("src/Contracts/Clean1.sharp").exists() && !root.join("src/Contracts/Clean1.php").exists());
        assert!(
            logged.contains(
                "The converted files could not be compiled for the extension hosts, so no hosts that load them proved the project, and git holds their plain PHP: "
            ),
            "{logged}"
        );
    }

    /// One of thirteen interfaces adds an issue in its plain PHP implementer, which points into the interface. The
    /// implementer also names the other twelve, so an edit of any of them makes the analysis re-analyze it, and only
    /// the file the issue points into tells the interface apart. The first proof finds the issue and sets the interface
    /// apart, and the second accepts the other twelve, so the first proof's issue keeps the one set apart plain PHP: two
    /// proofs. Halving the whole set took eight.
    #[test]
    fn an_issue_in_another_file_sets_apart_the_file_it_points_into() {
        let mut files: Vec<(String, String)> = (0..12).map(clean).collect();
        let [interface, (drain, code)] = sink(0);
        let named: Vec<String> =
            (0..12).map(|index| format!("\\Generated\\Contracts\\Clean{index} $clean{index}")).collect();
        let uses =
            format!("    public function uses({}): void\n    {{\n    }}\n\n    /** @param array", named.join(", "));
        files.extend([interface, (drain, code.replacen("    /** @param array", &uses, 1))]);

        let (exit_code, outcome) = convert_contracts(&files);

        assert_eq!(exit_code, ExitCode::SUCCESS);
        let refused: Vec<String> = outcome.refused.keys().map(|name| display(name)).collect();
        assert_eq!(refused, ["src/Contracts/Sink0.php"]);
        assert_eq!(outcome.converted.len(), 12);
        assert_eq!(outcome.proofs, 2);
    }

    /// A folder of 200 classes, each calling the one before it, converts in one proof of them all, so the proof's cost
    /// stays one analysis of the project, never one per file.
    #[test]
    fn two_hundred_classes_convert_in_one_proof_of_them_all() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let files: Vec<(String, String)> = (0..200)
            .map(|index| {
                let previous = match index {
                    0 => String::new(),
                    _ => format!(
                        "\n    public function previous(): Step{0}\n    {{\n        return new Step{0}();\n    }}\n",
                        index - 1
                    ),
                };
                (
                    format!("Step{index}.php"),
                    format!(
                        "<?php\n\nnamespace Generated;\n\nfinal class Step{index}\n{{\n    public function index(): int\n    {{\n        return {index};\n    }}\n{previous}}}\n"
                    ),
                )
            })
            .collect();
        let configuration = project(&root, &files);

        let (exit_code, outcome) = ConvertCommand { paths: vec![root.join("src")], dry_run: false }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert!(root.join("src/Step199.sharp").exists() && !root.join("src/Step199.php").exists());
        assert_eq!(outcome.converted.len(), 200);
        assert_eq!(outcome.proofs, 1);
    }

    /// Git is the undo, so a file git holds no commit of as it is, untracked or changed since, stays plain PHP.
    #[test]
    fn a_file_git_holds_no_commit_of_as_it_is_stays_plain_php() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = project(&root, &[clean(0), clean(1)]);
        let draft = clean(2);
        std::fs::write(root.join("src").join(&draft.0), &draft.1).unwrap();
        std::fs::write(root.join("src/Contracts/Clean1.php"), clean(1).1.replace("id()", "key()")).unwrap();

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(
            refused,
            [
                "src/Contracts/Clean1.php: git holds no commit of it as it is, so git, which undoes a conversion, can't give back its plain PHP",
                "src/Contracts/Clean2.php: git holds no commit of it as it is, so git, which undoes a conversion, can't give back its plain PHP",
            ]
        );
        assert_eq!(outcome.converted.len(), 1);
    }

    /// A path the command line names that the project's analysis doesn't load gets one message saying why, a file as
    /// a folder, and fails the run, since the run didn't reach what it was asked to convert. A run with nothing to
    /// prove makes no proof.
    #[test]
    fn each_named_path_the_analysis_does_not_load_says_why() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().canonicalize().unwrap().join("Outside.php");
        let class = "<?php\n\nnamespace Generated;\n\nfinal class Made\n{\n}\n";
        for (path, code) in [
            ("config/app.php", "<?php\n\nreturn ['name' => 'app'];\n"),
            ("database/seeders/DatabaseSeeder.php", class),
            ("tools/Helper.php", class),
            ("src/Generated/Made.php", class),
            ("src/Notes/readme.md", "notes\n"),
            ("src/Contracts/notes.txt", "notes\n"),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), code).unwrap();
        }
        std::fs::write(&outside, class).unwrap();
        project(&root, &[clean(0)]);
        std::fs::write(
            root.join("mago.toml"),
            "php-version = \"8.5\"\n\n[source]\npaths = [\"src\", \"tools/*Rule.php\"]\nexcludes = [\"src/Generated\"]\n",
        )
        .unwrap();
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root.clone()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();
        let paths = [
            "config",
            "database/seeders/DatabaseSeeder.php",
            "tools/Helper.php",
            "src/Contracts/Missing.php",
            "src/Generated/Made.php",
            "src/Notes",
            "src/Contracts/notes.txt",
        ];
        let mut named: Vec<PathBuf> = paths.iter().map(|path| root.join(path)).collect();
        named.push(outside.clone());

        let (exit_code, outcome) =
            ConvertCommand { paths: named, dry_run: true }.run(configuration, ColorChoice::Never).unwrap();

        assert_eq!(exit_code, ExitCode::FAILURE);
        assert_eq!(
            outcome.unreached,
            [
                "config is outside the source paths (src, tools/*Rule.php), so the project's analysis doesn't load it.".to_owned(),
                "database/seeders/DatabaseSeeder.php is outside the source paths (src, tools/*Rule.php), so the project's analysis doesn't load it.".to_owned(),
                "tools/Helper.php is outside the source paths (src, tools/*Rule.php), so the project's analysis doesn't load it.".to_owned(),
                "src/Contracts/Missing.php doesn't exist.".to_owned(),
                "src/Generated/Made.php is excluded from the project's analysis.".to_owned(),
                "src/Notes holds no plain PHP file the project's analysis loads.".to_owned(),
                "src/Contracts/notes.txt is no plain PHP file.".to_owned(),
                format!("{} is outside the workspace {}.", outside.display(), root.display()),
            ]
        );
        assert_eq!(outcome.proofs, 0);
    }

    /// A run whose every file is refused before its proof, here by its shape, starts no extension host, and succeeds:
    /// the file stays plain PHP with its reason.
    #[test]
    fn a_run_whose_every_file_is_refused_by_its_shape_starts_no_extension_host() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let script =
            ("Contracts/helpers.php".to_owned(), "<?php\n\nfunction helper(): int\n{\n    return 1;\n}\n".to_owned());
        project(&root, &[script]);
        let started = root.join("host-started");
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("tools/host.php"),
            "<?php\n\nfile_put_contents(getenv('STARTED'), \"started\\n\", FILE_APPEND);\n",
        )
        .unwrap();
        std::fs::write(
            root.join("mago.toml"),
            format!(
                "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n\n[extension-hosts.proof]\nfile = \"tools/host.php\"\nenvironment = {{ STARTED = {:?} }}\n",
                started.display().to_string()
            ),
        )
        .unwrap();
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root.clone()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();

        let run = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never);

        assert!(!started.exists(), "the run started the extension host");
        let (exit_code, outcome) = run.unwrap();
        assert_eq!(exit_code, ExitCode::SUCCESS);
        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(refused, ["src/Contracts/helpers.php: it declares no class, interface or enum, so it is a script"]);
        assert_eq!(outcome.proofs, 0);
    }

    /// A project at `root` whose class `Box` converts with a property only its own method reads, a method a plain PHP
    /// class of another namespace calls, and a method nothing uses.
    fn boxed(root: &Path) -> Configuration {
        let boxed = (
            "Contracts/Box.php".to_owned(),
            "<?php\n\nnamespace Generated\\Contracts;\n\nfinal class Box\n{\n    public function __construct(public int $item)\n    {\n    }\n\n    public function size(): int\n    {\n        return $this->item;\n    }\n\n    public function label(): string\n    {\n        return 'box';\n    }\n}\n".to_owned(),
        );
        let counter = (
            "Shop/Counter.php".to_owned(),
            "<?php\n\nnamespace Generated\\Shop;\n\nuse Generated\\Contracts\\Box;\n\nfinal class Counter\n{\n    public function count(Box $box): int\n    {\n        return $box->size();\n    }\n}\n".to_owned(),
        );
        let configuration = project(root, &[boxed, counter]);
        // A project's public PHP# member that nothing outside its namespace uses is an error, and a library's never.
        std::fs::write(
            root.join("composer.json"),
            r#"{"type": "project", "autoload": {"psr-4": {"Generated\\": "src/"}}}"#,
        )
        .unwrap();

        configuration
    }

    /// `Box` once converted: `item`, which only `Box` reads, is `private`, `size`, which `Counter` of another namespace
    /// calls, is `public`, and `label`, which nothing the analysis reads uses, is `internal`.
    const BOXED: &str = "namespace Generated.Contracts;\n\npublic final class Box\n{\n    public Box(private int item)\n    {\n    }\n\n    public int size()\n    {\n        return this.item;\n    }\n\n    internal string label()\n    {\n        return 'box';\n    }\n}\n";

    /// Each member of a project's class a dry run converts takes the narrowest access level its uses need, so the
    /// proof finds no member too wide.
    #[test]
    fn a_dry_run_gives_each_member_of_a_project_the_access_its_uses_need() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = boxed(&root);

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        assert_eq!(outcome.converted.len(), 1, "{:#?}", outcome.refused);
        assert_eq!(String::from_utf8_lossy(&outcome.converted[0].2), BOXED);
        let expected: Vec<&str> = outcome.expected.iter().map(|found| found.shown.as_str()).collect();
        assert!(expected.is_empty(), "{expected:#?}");
    }

    /// A run writes each member of a project's class with the narrowest access level its uses need, and the proof of
    /// the written files holds.
    #[test]
    fn a_run_writes_each_member_of_a_project_with_the_access_its_uses_need() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = boxed(&root);

        let (exit_code, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: false }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        assert_eq!(exit_code, ExitCode::SUCCESS, "{:#?}", outcome.refused);
        assert_eq!(std::fs::read_to_string(root.join("src/Contracts/Box.sharp")).unwrap(), BOXED);
        let expected: Vec<&str> = outcome.expected.iter().map(|found| found.written.as_str()).collect();
        assert!(expected.is_empty(), "{expected:#?}");
    }

    /// A library's function and constant of PHP's root namespace, beside the classes of `Acme\Foundation`, a function of
    /// a library with no class, which its Composer package's name gives `Acme\StringHelpers`, and the plain PHP class
    /// `name`, which calls and reads them in its `run()` as `body` does.
    fn root_names(name: &str, body: &str) -> [(String, String); 5] {
        [
            (
                "Lib/Foundation/Application.php".to_owned(),
                "<?php\n\nnamespace Acme\\Foundation;\n\nfinal class Application\n{\n}\n".to_owned(),
            ),
            (
                "Lib/Foundation/helpers.php".to_owned(),
                "<?php\n\nconst ACME_VERSION = '1.0';\n\nfunction abort(int $code): never\n{\n    throw new \\RuntimeException('Aborted.', $code);\n}\n".to_owned(),
            ),
            ("Lib/Strings/composer.json".to_owned(), r#"{"name": "acme/string-helpers", "type": "library"}"#.to_owned()),
            (
                "Lib/Strings/functions.php".to_owned(),
                "<?php\n\nfunction str_shout(string $text): string\n{\n    return $text . '!';\n}\n".to_owned(),
            ),
            (
                format!("Contracts/{name}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Contracts;\n\nfinal class {name}\n{{\n    public function run(): string\n    {{\n{body}\n    }}\n}}\n"
                ),
            ),
        ]
    }

    /// The `extern namespace` lines a converted file needs go in `src/Stubs/Externs.sharp`, the first PSR-4 folder's,
    /// which the run creates in that folder's `Stubs` namespace: one line for each folder, naming the first of its names
    /// the file meets, and a `uses any` line, with its import, for each function the file calls that no `extern` line
    /// declares. A run whose files need no line it lacks leaves the file as it is, and one that needs more adds those
    /// lines alone, each in its sorted place. Each converted file compiles with them, as its proof held them.
    #[test]
    fn the_extern_namespace_lines_converted_files_need_go_in_the_declarations_file_once() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let externs = root.join("src/Stubs/Externs.sharp");
        let run = |name: &str, body: &str| {
            let configuration = project(&root, &root_names(name, body));
            let path = root.join(format!("src/Contracts/{name}.php"));
            let (exit_code, outcome) =
                ConvertCommand { paths: vec![path], dry_run: false }.run(configuration, ColorChoice::Never).unwrap();
            assert_eq!(exit_code, ExitCode::SUCCESS);
            assert_eq!(outcome.converted.len(), 1, "{:#?}", outcome.refused);
            assert!(root.join(format!("src/Contracts/{name}.sharp")).exists());

            std::fs::read_to_string(&externs).unwrap()
        };

        let first = run("Pages", "        abort(404);");
        assert_eq!(
            first,
            "namespace Generated.Stubs;\n\nimport Acme.Foundation;\n\nextern namespace Acme.Foundation for abort;\n\nextern Foundation.abort uses any;\n"
        );

        let second = run("Banner", "        return ACME_VERSION;");
        assert_eq!(second, first);

        let third = run("Greeter", "        return str_shout(ACME_VERSION);");
        assert_eq!(
            third,
            "namespace Generated.Stubs;\n\nimport Acme.Foundation;\nimport Acme.StringHelpers;\n\nextern namespace Acme.Foundation for abort;\nextern namespace Acme.StringHelpers for str_shout;\n\nextern Foundation.abort uses any;\nextern StringHelpers.str_shout uses any;\n"
        );
    }

    /// Spec section 29: every plain PHP class a converted file reaches needs an `extern` line, so the run writes one that
    /// says `uses any` for each class the project's lines leave out, with its import, under its namespace's last part
    /// before its name where another import takes its name, and the converted file compiles with them.
    #[test]
    fn a_converted_file_that_calls_plain_php_classes_compiles_with_their_uses_any_lines() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let message = |namespace: &str| {
            format!(
                "<?php\n\nnamespace Acme\\{namespace};\n\nfinal class Message\n{{\n    public function text(): string\n    {{\n        return '{namespace}';\n    }}\n}}\n"
            )
        };
        let files = [
            (
                "Lib/Mail/Mailchimp.php".to_owned(),
                "<?php\n\nnamespace Acme\\Mail;\n\nfinal class Mailchimp\n{\n    public static function lists(): self\n    {\n        return new self();\n    }\n\n    public function name(): string\n    {\n        return 'lists';\n    }\n}\n".to_owned(),
            ),
            ("Lib/Mail/Message.php".to_owned(), message("Mail")),
            ("Lib/Chat/Message.php".to_owned(), message("Chat")),
            (
                "Contracts/Sync.php".to_owned(),
                "<?php\n\nnamespace Generated\\Contracts;\n\nfinal class Sync\n{\n    public function run(): string\n    {\n        return \\Acme\\Mail\\Mailchimp::lists()->name() . (new \\Acme\\Mail\\Message())->text() . (new \\Acme\\Chat\\Message())->text();\n    }\n}\n".to_owned(),
            ),
        ];
        let configuration = project(&root, &files);
        let path = root.join("src/Contracts/Sync.php");
        let (exit_code, outcome) =
            ConvertCommand { paths: vec![path], dry_run: false }.run(configuration, ColorChoice::Never).unwrap();

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert_eq!(outcome.converted.len(), 1, "{:#?}", outcome.refused);
        assert_eq!(
            std::fs::read_to_string(root.join("src/Stubs/Externs.sharp")).unwrap(),
            "namespace Generated.Stubs;\n\nimport Acme.Chat.Message;\nimport Acme.Mail.Mailchimp;\nimport Acme.Mail.Message as MailMessage;\n\nextern MailMessage uses any;\nextern Mailchimp uses any;\nextern Message uses any;\n"
        );
    }

    /// A class that converts is PHP#, which takes no `extern` line, so a later run that converts it takes away the line
    /// and the import an earlier run wrote for it, and the file compiles, as a class hierarchy converts in either order.
    #[test]
    fn a_run_that_converts_a_class_takes_away_the_line_an_earlier_run_wrote_for_it() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let files = [
            (
                "Lib/Mail/Mailchimp.php".to_owned(),
                "<?php\n\nnamespace Generated\\Lib\\Mail;\n\nfinal class Mailchimp\n{\n    public function name(): string\n    {\n        return 'lists';\n    }\n}\n".to_owned(),
            ),
            (
                "Contracts/Sync.php".to_owned(),
                "<?php\n\nnamespace Generated\\Contracts;\n\nfinal class Sync\n{\n    public function run(): string\n    {\n        return (new \\Generated\\Lib\\Mail\\Mailchimp())->name();\n    }\n}\n".to_owned(),
            ),
        ];
        let configuration = project(&root, &files);
        let externs = root.join("src/Stubs/Externs.sharp");
        let convert = |path: &str| {
            ConvertCommand { paths: vec![root.join(path)], dry_run: false }
                .run(configuration.clone(), ColorChoice::Never)
                .unwrap()
        };

        let (_, sync) = convert("src/Contracts/Sync.php");
        assert_eq!(sync.converted.len(), 1, "{:#?}", sync.refused);
        assert_eq!(
            std::fs::read_to_string(&externs).unwrap(),
            "namespace Generated.Stubs;\n\nimport Generated.Lib.Mail.Mailchimp;\n\nextern Mailchimp uses any;\n"
        );

        let (exit_code, mailchimp) = convert("src/Lib/Mail/Mailchimp.php");
        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert_eq!(mailchimp.converted.len(), 1, "{:#?}", mailchimp.refused);
        assert_eq!(std::fs::read_to_string(&externs).unwrap(), "namespace Generated.Stubs;\n");
    }

    /// The declarations file goes in the `Stubs` folder of the first PSR-4 folder `composer.json` lists, in its
    /// namespace, whatever the order of the namespaces.
    #[test]
    fn the_declarations_file_goes_in_the_first_psr4_folder_composer_json_lists() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("composer.json"), r#"{"autoload": {"psr-4": {"Zoo\\": "zoo/", "App\\": "app/"}}}"#)
            .unwrap();

        let roots = psr4_roots(&root).unwrap();
        assert_eq!(roots, [("Zoo".to_owned(), PathBuf::from("zoo")), ("App".to_owned(), PathBuf::from("app"))]);
        let externs = Externs::new(&root, &roots[0]).unwrap();
        assert_eq!(
            (display(&externs.name), externs.namespace),
            ("zoo/Stubs/Externs.sharp".to_owned(), "Zoo.Stubs".to_owned())
        );
    }

    /// A file the compile refuses once converted fails its proof with the compile's first error, and a converted file
    /// whose lowering panicked with the panic's text, a defect of the checker, whatever else the refusal names.
    #[test]
    fn a_refused_compile_names_its_first_error_and_a_lowering_that_panicked_names_the_panic() {
        let candidate = Candidate {
            name: b"src/Box.php".to_vec(),
            path: None,
            original: b"<?php\n\nnamespace App;\n\nfinal class Box\n{\n}\n".to_vec(),
            text: Vec::new(),
            extern_namespaces: Vec::new(),
            undeclared: Vec::new(),
            declares: Vec::new(),
        };
        let panicked = IssueCollection::from(vec![
            Issue::error("Invalid return.").with_code("invalid-return-statement"),
            Issue::error("A defect of the checker: boom").with_code(LOWERING_PANIC),
        ]);
        let linked = IssueCollection::from(vec![Issue::error("Shelf::size() must be compatible with Box::size().")]);
        let plain = Proof { issues: Issues::default(), refused: HashMap::new() };
        let proof = Proof {
            issues: Issues::default(),
            refused: HashMap::from([(b"src/Box.sharp".to_vec(), panicked), (b"src/Shelf.php".to_vec(), linked)]),
        };

        let judgement = judge(&plain, &proof, &[&candidate]);

        let shown: Vec<&str> = judgement.own[&candidate.name].iter().map(|found| found.shown.as_str()).collect();
        assert_eq!(shown, ["src/Box.php: mago compile would refuse it once converted: A defect of the checker: boom"]);
        let others: Vec<&str> = judgement.others.iter().map(|found| found.shown.as_str()).collect();
        assert_eq!(
            others,
            ["src/Shelf.php: mago compile would refuse it: Shelf::size() must be compatible with Box::size()."]
        );
    }

    /// A lowering that panicked in the plain project's proof, which the judgement leaves out with the file it refuses,
    /// names its panic, so the run fails on it, and a file refused for another error names none.
    #[test]
    fn a_lowering_that_panicked_in_the_plain_proof_names_its_panic() {
        let panicked = IssueCollection::from(vec![
            Issue::error("Invalid return.").with_code("invalid-return-statement"),
            Issue::error("The lowering of src/Box.sharp panicked: boom").with_code(LOWERING_PANIC),
        ]);
        let refused =
            IssueCollection::from(vec![Issue::error("Invalid return.").with_code("invalid-return-statement")]);
        let plain = Proof {
            issues: Issues::default(),
            refused: HashMap::from([(b"src/Box.sharp".to_vec(), panicked), (b"src/Shelf.sharp".to_vec(), refused)]),
        };

        assert_eq!(panics(&plain), ["The lowering of src/Box.sharp panicked: boom"]);
    }

    /// Two issues of one member with one message stay two, told apart by their line of the PHP#, and an issue outside
    /// any class-like keeps its line.
    #[test]
    fn issues_of_a_converted_file_keep_their_lines() {
        let candidate = Candidate {
            name: b"src/Box.php".to_vec(),
            path: None,
            original: b"<?php\n\nnamespace App;\n\nfinal class Box\n{\n    public function size(): int\n    {\n        return 1;\n    }\n}\n".to_vec(),
            text: Vec::new(),
            extern_namespaces: Vec::new(),
            undeclared: Vec::new(),
            declares: Vec::new(),
        };
        let issue = |line: u32, members: &[&str]| {
            let key = (b"src/Box.sharp".to_vec(), Level::Error, Some("code".to_owned()), "Wrong.".to_owned(), line);
            (key, (1, BTreeSet::new(), members.iter().map(|member| (*member).to_owned()).collect()))
        };
        let issues =
            Issues::from([issue(9, &["Box::size()", "Box"]), issue(10, &["Box::size()", "Box"]), issue(3, &[])]);
        let plain = Proof { issues: Issues::default(), refused: HashMap::new() };
        let proof = Proof { issues, refused: HashMap::new() };

        let judgement = judge(&plain, &proof, &[&candidate]);

        let shown: BTreeSet<&str> = judgement.own[&candidate.name].iter().map(|found| found.shown.as_str()).collect();
        assert_eq!(
            shown,
            BTreeSet::from([
                "src/Box.php, at line 3 of its PHP#: error code: Wrong.",
                "src/Box.php:7, in Box::size(), at line 9 of its PHP#: error code: Wrong.",
                "src/Box.php:7, in Box::size(), at line 10 of its PHP#: error code: Wrong.",
            ])
        );
        let written: BTreeSet<&str> =
            judgement.own[&candidate.name].iter().map(|found| found.written.as_str()).collect();
        assert_eq!(
            written,
            BTreeSet::from([
                "src/Box.sharp:3: error code: Wrong.",
                "src/Box.sharp:9, in Box::size(): error code: Wrong.",
                "src/Box.sharp:10, in Box::size(): error code: Wrong.",
            ])
        );
    }

    /// A file that changes between git's check and the proof, or that is gone by then, stays plain PHP, since git no
    /// longer holds the bytes the proof would convert.
    #[test]
    fn a_file_that_changes_after_git_is_read_stays_plain_php() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = project(&root, &[clean(0), clean(1), clean(2)]);
        let command = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true };

        let (outcome, checked) = command.check(&configuration, ColorChoice::Never).unwrap();
        std::fs::write(root.join("src/Contracts/Clean0.php"), clean(0).1.replace("id()", "key()")).unwrap();
        std::fs::remove_file(root.join("src/Contracts/Clean1.php")).unwrap();
        let (_, outcome) = command.convert_checked(configuration, ColorChoice::Never, outcome, checked).unwrap();

        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(
            refused,
            [
                "src/Contracts/Clean0.php: it changed since mago convert read git, so git, which undoes a conversion, can't give back its plain PHP",
                "src/Contracts/Clean1.php: it changed since mago convert read git, so git, which undoes a conversion, can't give back its plain PHP",
            ]
        );
        let converted: Vec<String> = outcome.converted.iter().map(|(name, ..)| display(name)).collect();
        assert_eq!(converted, ["src/Contracts/Clean2.php"]);
    }

    /// A file in a git submodule is another repository's, so it stays plain PHP and the run names the submodule.
    #[test]
    fn a_file_in_a_git_submodule_names_the_submodule() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("project");
        let library = directory.path().canonicalize().unwrap().join("library");
        std::fs::create_dir_all(library.join("Contracts")).unwrap();
        std::fs::write(library.join("Contracts/Clean0.php"), clean(0).1).unwrap();
        commit_all(&library);
        std::fs::create_dir(&root).unwrap();
        let configuration = project(&root, &[clean(1)]);
        let source = library.to_string_lossy().into_owned();
        for arguments in [
            &["-c", "protocol.file.allow=always", "submodule", "add", "--quiet", &source, "src/Library"][..],
            &["-c", "user.name=test", "-c", "user.email=test@localhost", "commit", "--quiet", "-m", "library"],
        ] {
            let output =
                mago_database::process::output(std::process::Command::new("git").args(arguments).current_dir(&root))
                    .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Library")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(
            refused,
            [
                "src/Library/Contracts/Clean0.php: it is in the git submodule `src/Library`, whose files another repository holds, so the workspace's commit holds none of its plain PHP"
            ]
        );
    }

    /// A converted file is written at its `.sharp` name before its plain file leaves, so a write that fails, here at a
    /// folder of that name, leaves the plain file where it was, where moving it first would have taken it away.
    #[test]
    fn a_write_that_fails_leaves_the_plain_file_where_it_was() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        project(&root, &[clean(0)]);
        std::fs::create_dir_all(root.join("src/Contracts/Clean0.sharp/kept")).unwrap();

        let (written, stopped) =
            write(&root, &[(&b"src/Contracts/Clean0.php"[..], &b"public interface Clean0 { }\n"[..])]);

        assert_eq!(written, 0);
        assert!(stopped.is_some());
        assert_eq!(std::fs::read_to_string(root.join("src/Contracts/Clean0.php")).unwrap(), clean(0).1);
    }

    /// Copies each file under `from` to the same place under `to`.
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// A project holds the standard library in `vendor/`, where the analysis loads its PHP# files as a package's. The
    /// proof builds the project's database as `mago compile` does, so those files are the project's and its compile
    /// lowers a converted call of `mb_strtoupper`, `text.uppercase()`, with the library's inline form, as `mago
    /// compile` lowers it.
    #[test]
    fn the_proof_lowers_with_the_standard_library_mago_compile_lowers_with() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(
            root.join("mago.toml"),
            "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\nincludes = [\"vendor\"]\nextensions = [\"php\", \"sharp\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("composer.json"),
            r#"{"type": "library", "autoload": {"psr-4": {"Generated\\": "src/"}}}"#,
        )
        .unwrap();
        let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("../composer");
        let package = root.join("vendor/heyjordanparker/php-sharp-composer");
        copy_tree(&library.join("library"), &package.join("library"));
        for file in ["composer.json", "native.php"] {
            std::fs::copy(library.join(file), package.join(file)).unwrap();
        }
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/Upper.php"),
            "<?php\n\nnamespace Generated;\n\nfinal class Upper\n{\n    public function loud(string $text): string\n    {\n        return mb_strtoupper($text, 'UTF-8');\n    }\n}\n",
        )
        .unwrap();
        commit_all(&root);
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root.clone()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();

        let externs = Externs::new(&root, &psr4_roots(&root).unwrap()[0]).unwrap();
        let (prover, _) = Prover::new(&configuration, ColorChoice::Never, root.clone(), externs).unwrap().unwrap();
        let session = prover.open(&[]).unwrap();
        let library: Vec<String> = session
            .server
            .database()
            .files()
            .filter(|file| file.is_standard_library && file.file_type == FileType::Host)
            .map(|file| display(&file.name))
            .collect();
        drop(session);
        let (exit_code, outcome) = ConvertCommand { paths: vec![root.join("src")], dry_run: true }
            .run(configuration.clone(), ColorChoice::Never)
            .unwrap();

        assert!(library.iter().any(|name| name.ends_with("Text/StringMembers.sharp")), "{library:#?}");
        assert_eq!(exit_code, ExitCode::SUCCESS, "{:#?}", outcome.refused);
        let text: Vec<String> = outcome.converted.iter().map(|(_, _, text)| display(text)).collect();
        assert!(text[0].contains("return text.uppercase();"), "{text:#?}");
    }
}
