//! `mago convert <paths>`: raises each plain PHP class file the paths name into PHP#, in place, as `Foo.php` becoming
//! `Foo.sharp`, or names each construct that keeps it plain PHP.
//!
//! The types come from the project's own analysis, run as `mago analyze` runs it. Before it writes a file, the command
//! analyzes the converted text against the whole project and compiles it in memory, and keeps the file plain PHP when
//! the converted file has any error or warning, another file gains an error or a warning decision 9 doesn't expect, or
//! the compile refuses a file it accepted before. The project's `ignore` entries and baseline hide an extension host
//! rule's issue there, as `mago compile` hides it, and never one of the language's (spec section 27). The extension
//! hosts load the project's classes, so each file it writes that their compile takes in starts the analysis again. Git
//! is the undo: the command moves each tracked file with `git mv`, so the index holds the rename alone, git's history
//! follows the file through it, and the `.php` file `mago compile` later writes at the old path is never the tracked
//! one.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::ColorChoice;
use clap::Parser;
use diffy::PatchFormatter;

use mago_allocator::LocalArena;
use mago_composer::AutoloadPsr4value;
use mago_composer::ComposerPackage;
use mago_converter::Input;
use mago_converter::Refusal;
use mago_converter::convert;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::loader::mark_sharp_file;
use mago_names::resolver::NameResolver;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_prelude::Prelude;
use mago_reporting::CompiledIgnoreSet;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_reporting::baseline::Baseline;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_syntax::cst::Statement;
use mago_syntax::parser::parse_file;

use crate::baseline::unserialize_baseline;
use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::compile::compile_for_hosts;
use crate::commands::compile::standard_library;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::consts::PRELUDE_BYTES;
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

/// Each issue of an analysis, with how many times it is reported.
type Issues = HashMap<IssueKey, usize>;

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
    own: BTreeMap<Vec<u8>, Vec<String>>,
    others: Vec<String>,
    expected: Vec<String>,
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
    refused: BTreeMap<Vec<u8>, Vec<String>>,
    converted: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    /// The warnings decision 9 expects, each as the summary lists it.
    expected: Vec<String>,
}

impl ConvertCommand {
    pub fn execute(self, mut configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
        configuration.extension_hosts = compile_for_hosts(&configuration)?.0;
        let workspace = configuration.source.workspace.clone();
        let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
        let roots = psr4_roots(&workspace)?;
        let named: Vec<PathBuf> = self.paths.iter().map(|path| relative(&workspace, path)).collect();
        let Some((prover, mut session)) = Prover::new(&configuration, color_choice, root)? else {
            return Ok(ExitCode::FAILURE.into());
        };
        let plain = session.proof(&prover)?;

        let mut outcome = Outcome::default();
        let mut reached: HashSet<&PathBuf> = HashSet::new();
        let mut files = Vec::new();
        for file in session.server.database().files() {
            let name = Path::new(std::str::from_utf8(&file.name).unwrap_or_default());
            let mut naming = named.iter().filter(|path| name.starts_with(path)).peekable();
            if file.file_type == FileType::Host && file.name.ends_with(b".php") && naming.peek().is_some() {
                reached.extend(naming);
                files.push(file);
            }
        }
        files.sort_by(|a, b| a.name.cmp(&b.name));
        let mut candidates = Vec::new();
        for file in files {
            let name = file.name.to_vec();
            if let Err(reason) = shape(&file, &roots) {
                outcome.refused.entry(name).or_default().push(format!("{}: {reason}", display(&file.name)));
                continue;
            }
            match session.convert(&file) {
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
                for (name, reasons) in &judgement.own {
                    unproven = true;
                    let name = display(&sharp_name(name));
                    tracing::error!(
                        "{name} adds issues once the extension hosts load it, and git holds its plain PHP:"
                    );
                    for reason in reasons {
                        tracing::error!("  {reason}");
                    }
                }
                for reason in &judgement.others {
                    unproven = true;
                    tracing::error!("The converted files add an issue with the hosts that load them: {reason}");
                }
            }
        }

        let unreached: Vec<String> =
            named.iter().filter(|path| !reached.contains(path)).map(|path| path.display().to_string()).collect();
        for path in &unreached {
            tracing::error!("{path} names no plain PHP file the project's analysis loads.");
        }
        outcome.report(self.dry_run, color_choice);

        let failed = !outcome.refused.is_empty() || !unreached.is_empty() || unproven;
        Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }.into())
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
    /// the rest are proven again. An issue in another file names no converted file, so the set is proven in halves, each
    /// on top of what is accepted, until one file is left to keep plain PHP.
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
            let mut judgement = judge(plain, &self.open(&swaps)?.proof(self)?, &swaps);
            let before = set.len();
            for (name, reasons) in std::mem::take(&mut judgement.own) {
                match set.iter().position(|&index| candidates[index].name == name) {
                    Some(position) => {
                        set.remove(position);
                        outcome.refused.entry(name).or_default().extend(reasons);
                    }
                    // A file proven before gains an issue only with the files this proof adds.
                    None => judgement.others.extend(reasons),
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
                let reasons = judgement.others.into_iter().map(|reason| format!("{reason}, which converting it adds"));
                outcome.refused.entry(candidates[index].name.clone()).or_default().extend(reasons);
                return Ok(());
            }

            let second = set.split_off(set.len() / 2);
            self.prove(plain, candidates, accepted, set, outcome)?;
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

    /// The PHP# text of `file`, or each construct that keeps it plain PHP, by its place.
    fn convert(&self, file: &File) -> Result<Vec<u8>, Vec<String>> {
        let arena = LocalArena::new();
        let program = parse_file(&arena, file);
        let names = NameResolver::new(&arena).resolve(program);
        let Some((_, artifacts)) = self.server.analyze_file_with_artifacts(file.id) else {
            return Err(vec![format!("{}: the analysis holds no such file", display(&file.name))]);
        };
        let input = Input { file, program, names: &names, codebase: self.server.codebase(), artifacts: &artifacts };

        convert(&input).map_err(|refusals| refusals.iter().map(|refusal| place(file, refusal)).collect())
    }
}

/// A fresh orchestrator with the project's extension hosts started, as `mago analyze` starts them, and the project's
/// database.
fn load(
    configuration: &Configuration,
    color_choice: ColorChoice,
) -> Result<(Orchestrator<'_>, Database<'static>), Error> {
    let mut orchestrator = create_orchestrator(configuration, color_choice, false, false, false);
    analyze_the_whole_workspace_without_paths(&mut orchestrator);
    orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
    if let Some(external_analyzer) = start_external_analyzer(
        &configuration.extension_hosts,
        configuration.php_version,
        configuration.threads,
        &configuration.analyzer.plugins,
        configuration.analyzer.disable_default_plugins,
    ) {
        orchestrator.set_external_analyzer_handle(external_analyzer);
    }
    let mut database = orchestrator.load_database(&configuration.source.workspace, true, None, None)?;
    database.merge_base(Prelude::decode_database(PRELUDE_BYTES).expect("Failed to decode embedded prelude"));
    let database = database.into_static();

    Ok((orchestrator, database))
}

/// What `proof`, of the project with `swaps` converted, found that `plain`, of the plain project, didn't: each issue
/// by its file, level, code, message and line, with duplicates counted, and each file `mago compile` refuses. Every
/// issue of a converted file counts but a note, and decision 9's warnings are expected wherever they appear.
fn judge(plain: &Proof, proof: &Proof, swaps: &[&Candidate]) -> Judgement {
    let owner = |file: &[u8]| swaps.iter().find(|candidate| sharp_name(&candidate.name) == file);
    let mut judgement = Judgement::default();
    for (key, count) in &proof.issues {
        let (file, level, code, message, line) = key;
        if count.saturating_sub(plain.issues.get(key).copied().unwrap_or(0)) == 0 || *level < Level::Warning {
            continue;
        }
        let code = code.clone().unwrap_or_default();
        let shown = format!("{}:{line}: {level} {code}: {message}", display(file));
        if *level == Level::Warning && EXPECTED_WARNINGS.contains(&code.as_str()) {
            judgement.expected.push(shown);
        } else if let Some(candidate) = owner(file) {
            judgement.own.entry(candidate.name.clone()).or_default().push(shown);
        } else {
            judgement.others.push(shown);
        }
    }
    for file in proof.refused.difference(&plain.refused) {
        let shown = format!("{}: mago compile would refuse it", display(file));
        match owner(file) {
            Some(candidate) => judgement.own.entry(candidate.name.clone()).or_default().push(shown),
            None => judgement.others.push(shown),
        }
    }

    judgement
}

impl Outcome {
    fn report(&self, dry_run: bool, color_choice: ColorChoice) {
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

/// Moves the plain PHP file `name` to its `.sharp` name, with `git mv` when git tracks it, then writes `text` there.
fn write(workspace: &Path, name: &[u8], text: &[u8]) -> Result<(), Error> {
    let (source, target) = (PathBuf::from(display(name)), PathBuf::from(display(&sharp_name(name))));
    if !git::move_tracked(workspace, &source, &target)? {
        let source = workspace.join(&source);
        std::fs::remove_file(&source).map_err(|error| DatabaseError::io(IOAction::DeleteFile, &source, error))?;
    }
    write_by_rename(&workspace.join(&target), text)?;

    Ok(())
}

/// Each issue of `issues` by its file, level, code, message and line, with how many times it is reported.
fn keys(server: &Server, issues: IssueCollection) -> Issues {
    let mut keys = Issues::default();
    for issue in issues.iter() {
        let (file, line) = issue
            .primary_span()
            .and_then(|span| {
                let file = server.database().get(&span.file_id).ok()?;
                Some((file.name.to_vec(), file.line_number(span.start.offset) + 1))
            })
            .unwrap_or_default();
        *keys.entry((file, issue.level, issue.code.clone(), issue.message.clone(), line)).or_default() += 1;
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
    let mut statements: Vec<&Statement> = Vec::new();
    for statement in &program.statements {
        match statement {
            Statement::Namespace(namespace) => statements.extend(namespace.statements().iter()),
            statement => statements.push(statement),
        }
    }
    let declared: Vec<&[u8]> = statements
        .iter()
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

    /// A folder of 200 classes, each calling the one before it, converts in one proof of them all, so the proof's cost
    /// stays one analysis of the project, never one per file.
    #[test]
    fn two_hundred_classes_convert_within_fifteen_seconds() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::write(root.join("mago.toml"), "php-version = \"8.5\"\n\n[source]\npaths = [\"src\"]\n").unwrap();
        std::fs::write(root.join("composer.json"), r#"{"autoload": {"psr-4": {"Generated\\": "src/"}}}"#).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        for index in 0..200 {
            let previous = match index {
                0 => String::new(),
                _ => format!(
                    "\n    public function previous(): Step{0}\n    {{\n        return new Step{0}();\n    }}\n",
                    index - 1
                ),
            };
            std::fs::write(
                root.join(format!("src/Step{index}.php")),
                format!(
                    "<?php\n\nnamespace Generated;\n\nfinal class Step{index}\n{{\n    public function index(): int\n    {{\n        return {index};\n    }}\n{previous}}}\n"
                ),
            )
            .unwrap();
        }
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root.clone()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();

        let start = Instant::now();
        let outcome = ConvertCommand { paths: vec![root.join("src")], dry_run: false }
            .execute(configuration, ColorChoice::Never)
            .unwrap();

        assert_eq!(outcome.exit_code, ExitCode::SUCCESS);
        assert!(root.join("src/Step199.sharp").exists() && !root.join("src/Step199.php").exists());
        // Measured on an Apple Silicon Mac: 3.8 s in this debug test. In release builds, the 200 classes took 1.6 s,
        // where proving each file alone took 7.1 s, and 20 s a file on a 1,200-file project with extension hosts.
        assert!(start.elapsed().as_secs() < 15, "200 classes took {:?}, past the 15 s bound", start.elapsed());
    }
}
