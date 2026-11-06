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
use mago_codex::metadata::CodebaseMetadata;
use mago_composer::AutoloadPsr4value;
use mago_composer::ComposerPackage;
use mago_converter::Input;
use mago_converter::Library;
use mago_converter::Refusal;
use mago_converter::convert;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::COMPANION_START;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::loader::mark_sharp_file;
use mago_database::matcher::ExclusionMatcher;
use mago_database::matcher::contains_glob_metacharacters;
use mago_names::resolver::NameResolver;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
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
use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::compile::compile_for_hosts;
use crate::commands::compile::project_database;
use crate::commands::compile::standard_library;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::error::Error;
use crate::extensions::start_external_analyzer;
use crate::utils::create_orchestrator;
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

/// The warnings a conversion adds by design, decision 9 of `docs/architecture/slices/convert.md`: a member the converter
/// wrote `public` to keep PHP's reach that nothing outside its namespace uses, and a plain PHP caller's PHPDoc type
/// that the converted class now declares itself.
const EXPECTED_WARNINGS: [&str; 2] = ["too-wide-member", "redundant-docblock-type"];

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
}

/// What one analysis and in-memory compile of the project found: each issue, and each file `mago compile` refuses.
struct Proof {
    issues: Issues,
    refused: HashSet<Vec<u8>>,
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
    /// The warnings decision 9 expects.
    expected: Vec<Found>,
    /// How many analyses proved the converted files, each a whole project's with fresh extension hosts.
    proofs: usize,
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
        let (database, _) = project_database(&whole_workspace(configuration, color_choice), workspace)?;

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
                    let reason =
                        format!("it is in the git submodule `{}`: run mago convert there", submodule.display());
                    refuse(&file.name, reason);
                } else if committed.files.contains(&name) {
                    checked.push((file.name.to_vec(), file.contents.to_vec()));
                } else {
                    refuse(
                        &file.name,
                        "git holds no commit of it as it is: commit it first, so git holds its plain PHP".to_owned(),
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
        // With no file to convert, a file stays plain PHP or a named path reaches none, since the command line names a
        // path at least, so the run fails.
        if checked.is_empty() {
            outcome.report(self.dry_run, color_choice);
            return Ok((ExitCode::FAILURE, outcome));
        }

        let workspace = configuration.source.workspace.clone();
        let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
        configuration.extension_hosts = compile_for_hosts(&configuration)?.0;
        let Some((prover, mut session)) = Prover::new(&configuration, color_choice, root)? else {
            outcome.report(self.dry_run, color_choice);
            return Ok((ExitCode::FAILURE, outcome));
        };
        let plain = session.proof(&prover)?;
        // The forms come from the library files the proof's compile accepts, so a converted call runs what it does.
        let database = session.server.database().read_only();
        let forms = session.server.standard_library_forms(|issues| prover.hidden.shown(issues, &database))?;
        let library = Library::new(&forms, session.server.codebase());

        let mut candidates = Vec::new();
        for (name, held) in checked {
            // A file changed or gone since git was read holds bytes git doesn't, so it is no longer git's to undo.
            let database = session.server.database();
            let Some(file) = database.get_ref(&FileId::new(&name)).ok().filter(|file| *file.contents == *held) else {
                let reason = format!("{}: it changed since mago convert read git: commit it first", display(&name));
                outcome.refused.entry(name).or_default().push(reason);
                continue;
            };
            match session.convert(file, &library) {
                Ok(text) => {
                    candidates.push(Candidate { name, path: file.path.clone(), original: file.contents.to_vec(), text })
                }
                Err(reasons) => outcome.refused.entry(name).or_default().extend(reasons),
            }
        }
        drop(session);

        let mut accepted = Vec::new();
        prover.prove(&plain, &candidates, &mut accepted, (0..candidates.len()).collect(), &mut outcome)?;
        let accepted: Vec<&Candidate> = accepted.into_iter().map(|index| &candidates[index]).collect();
        outcome.converted = accepted
            .iter()
            .map(|candidate| (candidate.name.clone(), candidate.original.clone(), candidate.text.clone()))
            .collect();

        let mut unproven = false;
        if !self.dry_run && !accepted.is_empty() {
            for candidate in &accepted {
                write(&workspace, &candidate.name, &candidate.text)?;
            }
            // The extension hosts load the project's own classes, so when they load a converted one, hosts that see it
            // prove the project again (spec section 27).
            if compile_for_hosts(&configuration)?.1 {
                tracing::info!("The extension hosts load the converted files, so fresh hosts prove the project again.");
                let proof = prover.open(&[])?.proof(&prover)?;
                let judgement = judge(&plain, &proof, &accepted);
                for (name, found) in &judgement.own {
                    unproven = true;
                    let name = display(&sharp_name(name));
                    tracing::error!(
                        "{name} adds issues once the extension hosts load it, and git holds its plain PHP:"
                    );
                    for found in found {
                        tracing::error!("  {}", found.written);
                    }
                }
                for found in &judgement.others {
                    unproven = true;
                    tracing::error!(
                        "The converted files add an issue with the hosts that load them: {}",
                        found.written
                    );
                }
            }
        }

        outcome.report(self.dry_run, color_choice);

        let failed = !outcome.refused.is_empty() || !outcome.unreached.is_empty() || unproven;
        Ok((if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }, outcome))
    }
}

impl<'configuration> Prover<'configuration> {
    /// The prover of the project, and a session of the plain project. `None` when the standard library's package can't
    /// be found, which the log names.
    fn new(
        configuration: &'configuration Configuration,
        color_choice: ColorChoice,
        root: PathBuf,
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
        let prover = Self { configuration, color_choice, root, hidden, standard_library };
        let session = Session::new(orchestrator, database);

        Ok(Some((prover, session)))
    }

    /// A session of the project with fresh extension hosts, each of `swaps` in place of its plain file.
    fn open(&self, swaps: &[&Candidate]) -> Result<Session<'configuration>, Error> {
        let (orchestrator, mut database) = load(self.configuration, self.color_choice)?;
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
    /// the rest are proven again. An issue in another file sends the set to be proven in two parts, each on top of what
    /// is accepted, until one file is left to keep plain PHP. The second part is the second half of the fewest files
    /// one issue can come from, as [`Session::reached`] finds them, and the first part is the rest of the set. When no
    /// file of the set can bring an issue, the parts are the set's halves. When the first part is accepted whole and
    /// the second is one file, the proof that split them was that file on top of what is accepted, so its issues keep
    /// the file plain PHP with no proof more. Every proof is a whole analysis with fresh hosts, so each split costs a
    /// proof.
    fn prove(
        &self,
        plain: &Proof,
        candidates: &[Candidate],
        accepted: &mut Vec<usize>,
        mut set: Vec<usize>,
        outcome: &mut Outcome,
    ) -> Result<(), Error> {
        while !set.is_empty() {
            let swaps: Vec<&Candidate> = accepted.iter().chain(&set).map(|&index| &candidates[index]).collect();
            let start = Instant::now();
            let mut session = self.open(&swaps)?;
            let mut judgement = judge(plain, &session.proof(self)?, &swaps);
            outcome.proofs += 1;
            tracing::debug!(
                "Proof {} of {} files on {} accepted ran in {:?}: {} files with issues of their own, {} issues in others.",
                outcome.proofs,
                set.len(),
                accepted.len(),
                start.elapsed(),
                judgement.own.len(),
                judgement.others.len(),
            );
            let before = set.len();
            for (name, found) in std::mem::take(&mut judgement.own) {
                match set.iter().position(|&index| candidates[index].name == name) {
                    Some(position) => {
                        set.remove(position);
                        outcome.refused.entry(name).or_default().extend(found.into_iter().map(|found| found.shown));
                    }
                    // A file proven before gains an issue only with the files this proof adds.
                    None => judgement.others.extend(found),
                }
            }
            if set.len() < before {
                continue;
            }
            if judgement.others.is_empty() {
                accepted.append(&mut set);
                outcome.expected = judgement.expected;
                return Ok(());
            }
            if let [index] = set[..] {
                outcome.adds(&candidates[index].name, judgement.others);
                return Ok(());
            }

            let narrowest = session
                .reached(&judgement.others, candidates, &set)
                .into_iter()
                .filter(|reached| !reached.is_empty())
                .min_by_key(Vec::len);
            let second = match narrowest {
                Some(mut suspects) => {
                    let second = suspects.split_off(suspects.len() / 2);
                    set.retain(|index| !second.contains(index));
                    second
                }
                None => set.split_off(set.len() / 2),
            };
            let (first, before) = (set.len(), accepted.len());
            self.prove(plain, candidates, accepted, set, outcome)?;
            // With the first part accepted whole, this proof was the one file of the second on top of what is
            // accepted, so its issues keep that file plain PHP with no proof more.
            if let [index] = second[..]
                && accepted.len() == before + first
            {
                outcome.adds(&candidates[index].name, judgement.others);
                return Ok(());
            }
            return self.prove(plain, candidates, accepted, second, outcome);
        }

        Ok(())
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
        let compilations = self.server.compile(
            |path| unit::stamp(&prover.root, path),
            |_| Ok(Default::default()),
            &prover.standard_library,
            |issues| prover.hidden.shown(issues, &database),
        )?;
        let refused = compilations
            .into_iter()
            .filter(|(_, compilation)| matches!(compilation, Compilation::Refused(_)))
            .filter_map(|(file_id, _)| database.get(&file_id).ok().map(|file| file.name.to_vec()))
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

    /// The PHP# text of `file`, or each construct that keeps it plain PHP, by its place. A built-in's call becomes the
    /// member of `library` that runs it.
    fn convert(&self, file: &File, library: &Library) -> Result<Vec<u8>, Vec<String>> {
        let arena = LocalArena::new();
        let program = parse_file(&arena, file);
        let names = NameResolver::new(&arena).resolve(program);
        let Some((_, artifacts)) = self.server.analyze_file_with_artifacts(file.id) else {
            return Err(vec![format!("{}: the analysis holds no such file", display(&file.name))]);
        };
        let input =
            Input { file, program, names: &names, codebase: self.server.codebase(), artifacts: &artifacts, library };

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
    let orchestrator = whole_workspace(configuration, color_choice);
    if let Some(external_analyzer) = start_external_analyzer(
        &configuration.extension_hosts,
        configuration.php_version,
        configuration.threads,
        &configuration.analyzer.plugins,
        configuration.analyzer.disable_default_plugins,
    ) {
        orchestrator.set_external_analyzer_handle(external_analyzer);
    }
    let (database, _) = project_database(&orchestrator, &configuration.source.workspace)?;

    Ok((orchestrator, database))
}

/// A fresh orchestrator of the whole workspace, as `mago analyze` makes one when no path is named. It starts no
/// extension host, so the database it loads holds the project's files as [`load`]'s does, without the hosts' own.
fn whole_workspace(configuration: &Configuration, color_choice: ColorChoice) -> Orchestrator<'_> {
    let mut orchestrator = create_orchestrator(configuration, color_choice, false, false, false);
    analyze_the_whole_workspace_without_paths(&mut orchestrator);
    orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());

    orchestrator
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
    for file in proof.refused.difference(&plain.refused) {
        let written = format!("{}: mago compile would refuse it", display(file));
        let found = |shown: String| Found { shown, written: written.clone(), files: BTreeSet::from([file.clone()]) };
        match owner(file) {
            Some(candidate) => judgement
                .own
                .entry(candidate.name.clone())
                .or_default()
                .push(found(format!("{}: mago compile would refuse it once converted", display(&candidate.name)))),
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
        if dry_run {
            let mut formatter = PatchFormatter::new();
            if should_use_colors(color_choice) {
                formatter = formatter.with_color();
            }
            for (name, original, text) in &self.converted {
                let (original, text) = (String::from_utf8_lossy(original), String::from_utf8_lossy(text));
                let patch = diffy::create_patch(&original, &text);
                println!("{} -> {}", display(name), display(&sharp_name(name)));
                println!("{}", formatter.fmt_patch(&patch));
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

/// Writes `text` at the `.sharp` name of the committed plain PHP file `name`, then records the rename in git's index,
/// as `git mv` would, and only then removes the plain file, so a write that fails leaves it where it was.
fn write(workspace: &Path, name: &[u8], text: &[u8]) -> Result<(), Error> {
    let (source, target) = (PathBuf::from(display(name)), PathBuf::from(display(&sharp_name(name))));
    write_by_rename(&workspace.join(&target), text)?;
    git::stage_rename(workspace, &source, &target)?;
    let source = workspace.join(&source);
    std::fs::remove_file(&source).map_err(|error| DatabaseError::io(IOAction::DeleteFile, &source, error))?;

    Ok(())
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
fn shape(file: &File, roots: &[PathBuf]) -> Result<(), String> {
    let name = display(&file.name);
    if !roots.iter().any(|folder| Path::new(&name).starts_with(folder)) {
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
            if let Some(method) = codebase.get_method(class.name.as_bytes(), method.as_bytes())
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
    if std::fs::read(&on_disk).is_ok_and(|contents| contents.starts_with(COMPANION_START.as_bytes())) {
        let sharp = path.with_extension("sharp");
        return Ok(format!("{shown} is what mago compile writes for `{}`, which is already PHP#.", sharp.display()));
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

/// Each PSR-4 folder of `composer.json`'s `autoload`, relative to the workspace. A test under `autoload-dev` stays
/// out, since a test runner loads a test only from the file it lists (spec section 27).
fn psr4_roots(workspace: &Path) -> Result<Vec<PathBuf>, Error> {
    let path = workspace.join("composer.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let composer: ComposerPackage = contents.parse()?;
    let mut roots = Vec::new();
    for (_, value) in composer.autoload.map(|autoload| autoload.psr_4).unwrap_or_default() {
        let folders = match value {
            AutoloadPsr4value::String(folder) => vec![folder],
            AutoloadPsr4value::Array(folders) => folders,
        };
        roots.extend(folders.iter().map(|folder| PathBuf::from(folder.trim_end_matches('/'))));
    }

    Ok(roots)
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
            let output = std::process::Command::new("git").args(arguments).current_dir(root).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
    }

    /// The configuration of a project at `root` of `files`, each a path under `src/` and its code, with `src/` the
    /// PSR-4 folder of `Generated\`, all committed to git.
    fn project(root: &Path, files: &[(String, String)]) -> Configuration {
        std::fs::write(root.join("mago.toml"), "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n").unwrap();
        std::fs::write(root.join("composer.json"), r#"{"autoload": {"psr-4": {"Generated\\": "src/"}}}"#).unwrap();
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

    /// An interface `Named{index}` whose PHPDoc gives `name()` a `non-empty-string`, which PHP# writes `string`, and its
    /// plain PHP caller `Greeter{index}`, which passes the name where a `non-empty-string` goes. Once the interface
    /// converts, the caller gains an issue that points into no converted file.
    fn named(index: usize) -> [(String, String); 2] {
        [
            (
                format!("Contracts/Named{index}.php"),
                format!(
                    "<?php\n\nnamespace Generated\\Contracts;\n\ninterface Named{index}\n{{\n    /** @return non-empty-string */\n    public function name(): string;\n}}\n"
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

        assert_eq!(exit_code, ExitCode::FAILURE);
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

        assert_eq!(exit_code, ExitCode::FAILURE);
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

        assert_eq!(exit_code, ExitCode::FAILURE);
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
                "src/Contracts/Clean1.php: git holds no commit of it as it is: commit it first, so git holds its plain PHP",
                "src/Contracts/Clean2.php: git holds no commit of it as it is: commit it first, so git holds its plain PHP",
            ]
        );
        assert_eq!(outcome.converted.len(), 1);
    }

    /// A path the command line names that the project's analysis doesn't load gets one message saying why, a file as
    /// a folder, and a run with nothing to prove makes no proof.
    #[test]
    fn each_named_path_the_analysis_does_not_load_says_why() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().canonicalize().unwrap().join("Outside.php");
        let class = "<?php\n\nnamespace Generated;\n\nfinal class Made\n{\n}\n";
        let written = format!("{COMPANION_START}src/Contracts/Order.sharp\n");
        for (path, code) in [
            ("config/app.php", "<?php\n\nreturn ['name' => 'app'];\n"),
            ("database/seeders/DatabaseSeeder.php", class),
            ("tools/Helper.php", class),
            ("src/Generated/Made.php", class),
            ("src/Notes/readme.md", "notes\n"),
            ("src/Contracts/notes.txt", "notes\n"),
            ("src/Contracts/Order.sharp", "namespace Generated.Contracts;\n\npublic interface Order\n{\n}\n"),
            ("src/Contracts/Order.php", &written),
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
            "src/Contracts/Order.php",
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
                "src/Contracts/Order.php is what mago compile writes for `src/Contracts/Order.sharp`, which is already PHP#.".to_owned(),
                format!("{} is outside the workspace {}.", outside.display(), root.display()),
            ]
        );
        assert_eq!(outcome.proofs, 0);
    }

    /// A run whose every file is refused before its proof, here by its shape, starts no extension host.
    #[test]
    fn a_run_whose_every_file_is_refused_by_its_shape_starts_no_extension_host() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let script =
            ("Contracts/helpers.php".to_owned(), "<?php\n\nfunction helper(): int\n{\n    return 1;\n}\n".to_owned());
        project(&root, &[script]);
        let started = root.join("host-started");
        std::fs::write(
            root.join("mago.toml"),
            format!(
                "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n\n[extension-hosts.proof]\ncommand = [\"sh\", \"-c\", \"echo started >> '{}'\"]\nworkers = 1\n",
                started.display()
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
        assert_eq!(exit_code, ExitCode::FAILURE);
        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(refused, ["src/Contracts/helpers.php: it declares no class, interface or enum, so it is a script"]);
        assert_eq!(outcome.proofs, 0);
    }

    /// A project at `root` whose class `Box` converts with a public property and a public method nothing outside its
    /// namespace uses, which warn once converted, as decision 9 expects.
    fn boxed(root: &Path) -> Configuration {
        let boxed = (
            "Contracts/Box.php".to_owned(),
            "<?php\n\nnamespace Generated\\Contracts;\n\nfinal class Box\n{\n    public function __construct(public int $item)\n    {\n    }\n\n    public function size(): int\n    {\n        return $this->item;\n    }\n}\n".to_owned(),
        );
        let configuration = project(root, &[boxed]);
        // A project's public PHP# members warn when nothing outside their namespace uses them, a library's never.
        std::fs::write(
            root.join("composer.json"),
            r#"{"type": "project", "autoload": {"psr-4": {"Generated\\": "src/"}}}"#,
        )
        .unwrap();

        configuration
    }

    /// An issue in a converted file a dry run doesn't write names the plain file the user has, at the member it is in
    /// and that member's line in the plain file, and at its line of the PHP# the diff shows.
    #[test]
    fn an_issue_in_a_converted_file_a_dry_run_leaves_names_its_plain_file_and_member() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = boxed(&root);

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        assert_eq!(outcome.converted.len(), 1, "{:#?}", outcome.refused);
        let mut expected: Vec<&str> = outcome.expected.iter().map(|found| found.shown.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(
            expected,
            [
                "src/Contracts/Box.php:11, in Box::size(), at line 9 of its PHP#: warning too-wide-member: `Box.size` is public, and nothing outside `Generated.Contracts` uses it.",
                "src/Contracts/Box.php:7, in Box::$item, at line 5 of its PHP#: warning too-wide-member: `Box.item` is public, and nothing outside `Generated.Contracts` uses it.",
            ]
        );
    }

    /// An issue in a converted file a run writes names the `.sharp` file, at its own line, with the member it is in.
    #[test]
    fn an_issue_in_a_converted_file_a_run_writes_names_its_sharp_file_and_member() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let configuration = boxed(&root);

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Contracts")], dry_run: false }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        let written = std::fs::read_to_string(root.join("src/Contracts/Box.sharp")).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        let mut expected: Vec<&str> = outcome.expected.iter().map(|found| found.written.as_str()).collect();
        expected.sort_unstable();
        assert_eq!(
            expected,
            [
                "src/Contracts/Box.sharp:5, in Box::$item: warning too-wide-member: `Box.item` is public, and nothing outside `Generated.Contracts` uses it.",
                "src/Contracts/Box.sharp:9, in Box::size(): warning too-wide-member: `Box.size` is public, and nothing outside `Generated.Contracts` uses it.",
            ],
            "{written}"
        );
        // Each line the warnings name declares its member in the written file.
        assert!(lines[4].contains("int item") && lines[8].contains("int size()"), "{written}");
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
        };
        let issue = |line: u32, members: &[&str]| {
            let key = (b"src/Box.sharp".to_vec(), Level::Error, Some("code".to_owned()), "Wrong.".to_owned(), line);
            (key, (1, BTreeSet::new(), members.iter().map(|member| (*member).to_owned()).collect()))
        };
        let issues =
            Issues::from([issue(9, &["Box::size()", "Box"]), issue(10, &["Box::size()", "Box"]), issue(3, &[])]);
        let plain = Proof { issues: Issues::default(), refused: HashSet::new() };
        let proof = Proof { issues, refused: HashSet::new() };

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
                "src/Contracts/Clean0.php: it changed since mago convert read git: commit it first",
                "src/Contracts/Clean1.php: it changed since mago convert read git: commit it first",
            ]
        );
        let converted: Vec<String> = outcome.converted.iter().map(|(name, ..)| display(name)).collect();
        assert_eq!(converted, ["src/Contracts/Clean2.php"]);
    }

    /// A file in a git submodule is another repository's, so it stays plain PHP and the run names where to convert it.
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
            let output = std::process::Command::new("git").args(arguments).current_dir(&root).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }

        let (_, outcome) = ConvertCommand { paths: vec![root.join("src/Library")], dry_run: true }
            .run(configuration, ColorChoice::Never)
            .unwrap();

        let refused: Vec<String> = outcome.refused.values().flatten().cloned().collect();
        assert_eq!(
            refused,
            ["src/Library/Contracts/Clean0.php: it is in the git submodule `src/Library`: run mago convert there"]
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

        let written = write(&root, b"src/Contracts/Clean0.php", b"public interface Clean0 { }\n");

        assert!(written.is_err());
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
        std::fs::write(root.join("composer.json"), r#"{"autoload": {"psr-4": {"Generated\\": "src/"}}}"#).unwrap();
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

        let (prover, _) = Prover::new(&configuration, ColorChoice::Never, root.clone()).unwrap().unwrap();
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
