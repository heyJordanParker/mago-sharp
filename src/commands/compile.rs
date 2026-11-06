//! `mago compile`: checks every PHP# file, the ones in `vendor/` too, and writes the `.sharpc` file the engine runs
//! for each accepted one.
//!
//! Every compiled file lives in one `.sharp` folder at the workspace root, at the real path of its source relative to
//! the root, with `.sharp` replaced by `.sharpc`. The folder mirrors the accepted sources exactly: a refused, removed
//! or renamed source loses its compiled file.
//!
//! Beside each of the project's own PHP# sources that declares a type, outside `vendor/` and `autoload-dev` folders, it
//! also writes a PHP file of the type's name that loads the source and states the type's declaration, so PHP code that
//! finds classes by listing `*.php` files or reading their source finds PHP# classes too, spec section 27. A removed
//! source loses its PHP file, and git's `info/exclude` keeps the files no `.gitignore` covers out of git.
//!
//! A compile that succeeds keeps a record of everything it read and wrote in the `.sharp` folder, and the next compile
//! checks it before any analysis: when every input and output is as it was, it prints the same summary and does nothing
//! else. A project with an extension host enabled compiles every time, since a host names none of what it reads.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;
use serde::Deserialize;
use serde::Serialize;

use mago_allocator::LocalArena;
use mago_build_id::BUILD_ID;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::GlobSettings;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::COMPANION_START;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_database::file::FolderNamespace;
use mago_database::membership::WorkspaceMatcher;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::OrchestratorError;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_orchestrator::service::incremental_analysis::compile::DispatcherModule;
use mago_orchestrator::service::incremental_analysis::compile::Listener;
use mago_prelude::Prelude;
use mago_reporting::CompiledIgnoreSet;
use mago_reporting::IssueCollection;
use mago_reporting::ReportingFormat;
use mago_reporting::ReportingTarget;
use mago_reporting::color::ColorChoice as ReportingColorChoice;
use mago_reporting::reporter::Reporter;
use mago_reporting::reporter::ReporterConfig;
use mago_semantics::is_php_keyword;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_sharp_lean::Lean;
use mago_sharp_lean::PACKAGE_FOLDER;
use mago_syntax::cst::Statement;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;

use crate::baseline::unserialize_baseline;
use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::args::reporting::default_reporting_format;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::config::extension::ExtensionHostConfiguration;
use crate::consts::PRELUDE_BYTES;
use crate::error::Error;
use crate::extensions::start_external_analyzer;
use crate::utils::create_orchestrator;
use crate::utils::git;
use crate::utils::write_by_rename;

/// The folder Composer installs packages into.
const VENDOR: &[u8] = b"vendor/";

/// The listener table in the `.sharp` folder, which the engine reads to run each PHP# listener, spec section 15. Every
/// compile of a project with a PHP# file writes it, listeners or none, so the engine can tell when it is out of date.
const LISTENERS: &str = "listeners.php";

/// The record of everything the last compile that succeeded read and wrote, in the `.sharp` folder and in the hosts'
/// folder, which the next compile checks before any analysis: when every input and output is as it was, the compile has
/// nothing to do. A refused, failed or interrupted compile leaves none.
const INPUTS: &str = "inputs.json";

/// Command that compiles every PHP# file into the `.sharp` folder.
#[derive(Parser, Debug, Default)]
#[command(name = "compile")]
pub struct CompileCommand {}

impl CompileCommand {
    /// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every
    /// other file in the `.sharp` folder.
    ///
    /// Fails when a file is refused, a plain PHP file whose class doesn't satisfy a PHP# type it extends or implements
    /// included, when a source is a link to a file outside the workspace, when a package in `vendor/` holds a `.sharp`
    /// folder of its own, or when a PHP file someone wrote sits where a source's PHP file goes. Every accepted file is
    /// written either way.
    pub fn execute(self, mut configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
        configuration.extension_hosts = compile_for_hosts(&configuration)?.0;

        compile(&configuration, color_choice)
    }
}

/// The environment variable that names the project root to a worker of an extension host, which the engine reads.
const HOST_ROOT: &str = "SHARP_ROOT";

/// The environment variable that names to a worker of an extension host the folder its engine reads the project's
/// compiled files from.
const HOST_COMPILED: &str = "SHARP_COMPILED";

/// The checker's report of the files the hosts' compile refused, in the hosts' folder, which the engine reads for a
/// worker's refusal of such a file.
const HOST_REPORT: &str = "report.json";

/// Compiles every PHP# file of the project for its extension hosts, so a host loads the project's own classes as on
/// plain PHP, on a fresh or read-only tree and after any change. Returns the project's hosts, each enabled one given
/// the folder through [`HOST_ROOT`] and [`HOST_COMPILED`], which every command starts its hosts from, and whether a
/// file in the folder changed, so a watch knows its hosts' workers may hold classes that changed. The folder is the
/// project's in mago's cache, outside the project, and only the hosts' engine reads it, so the project's `.sharp`
/// folder holds only what `mago compile` accepted.
///
/// The compile runs without the hosts and lowers every file that lowers, whatever the analysis says of it: a host sees
/// the project as it is, as a Roslyn analyzer gets the compilation with its errors and `tsc` emits despite type errors.
/// A file that does not lower is in the folder's [`HOST_REPORT`], so a worker that loads it gets the checker's errors.
/// It runs only when a file is missing from the folder or out of date, and prints nothing: the run with the hosts that
/// follows reports the project's errors.
///
/// # Errors
///
/// Returns the error that stops the compile.
pub(crate) fn compile_for_hosts(
    configuration: &Configuration,
) -> Result<(BTreeMap<String, ExtensionHostConfiguration>, bool), Error> {
    let mut hosts = configuration.extension_hosts.clone();
    if !hosts.values().any(|host| host.enabled) {
        return Ok((hosts, false));
    }

    let workspace = configuration.source.workspace.clone();
    let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
    let project: String =
        unit::source_hash(root.as_os_str().as_encoded_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    let folder = mago_sharp_lean::cache_root()
        .map_err(|error| OrchestratorError::General(error.to_string()))?
        .join("hosts")
        .join(project);
    let mut without_hosts = configuration.clone();
    without_hosts.extension_hosts.clear();

    let compiled = tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        compile_into(&without_hosts, &root, &folder)
    })?;

    for host in hosts.values_mut().filter(|host| host.enabled) {
        host.environment.insert(HOST_ROOT.to_owned(), root.to_string_lossy().into_owned());
        host.environment.insert(HOST_COMPILED.to_owned(), folder.to_string_lossy().into_owned());
    }

    Ok((hosts, compiled))
}

/// Compiles the project at `root` into `folder` for its extension hosts, as [`compile_for_hosts`] says, unless every
/// file there is current, and keeps the record of what it read and wrote in `folder`'s [`INPUTS`]. Returns whether a
/// file in `folder` changed.
fn compile_into(configuration: &Configuration, root: &Path, folder: &Path) -> Result<bool, Error> {
    let record_path = folder.join(INPUTS);
    let sources = source_files(configuration, root);
    let record = Record::start(configuration, root, &sources);
    let mut orchestrator = create_orchestrator(configuration, ColorChoice::Never, false, false, false);
    analyze_the_whole_workspace_without_paths(&mut orchestrator);
    orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
    let (database, _) = project_database(&orchestrator, &configuration.source.workspace)?;
    if every_compiled_file_is_current(configuration, root, folder, &sources, &database, |path| unit::stamp(root, path))?
    {
        return Ok(false);
    }
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);

    let library = standard_library(root, &database).unwrap_or_default();
    let mut server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator));
    server.analyze()?;
    // A file whose errors the lowering does not survive is refused, whatever panic message its lowering printed.
    let printing = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic| tracing::debug!("{panic}")));
    let mut stamped = Vec::new();
    let compilations = server.compile(
        |path| {
            let input = unit::stamp(root, path)?;
            stamped.push(recorded(path, input.as_ref()));
            Ok(input)
        },
        |_| Ok(Default::default()),
        &library,
        |_| IssueCollection::default(),
    );
    std::panic::set_hook(printing);
    let database = server.database();

    // Whether a file a host's worker loads changed, so a watch knows to start the workers again.
    let mut changed = false;
    let mut write = |path: &Path, bytes: &[u8]| -> Result<(), Error> {
        if std::fs::read(path).ok().as_deref() != Some(bytes) {
            write_by_rename(path, bytes)?;
            changed = true;
        }
        Ok(())
    };
    let mut written = BTreeSet::new();
    let mut refused = IssueCollection::default();
    let mut accepted = HashSet::new();
    for (file_id, compilation) in compilations? {
        let file = database.get_ref(&file_id)?;
        let source = file.path.clone().unwrap_or_else(|| root.join(String::from_utf8_lossy(&file.name).as_ref()));
        let Some(compiled) = host_compiled_path(root, folder, &source)? else {
            continue;
        };
        match compilation {
            Compilation::Accepted(bytes) => {
                write(&compiled, &bytes)?;
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Refused(errors) => refused.extend(errors),
        }
    }

    let inputs = listener_inputs(root, database)?;
    if !inputs.is_empty() {
        let listeners = server.listeners();
        let accepted: Vec<&Listener> =
            listeners.iter().filter(|listener| accepted.contains(&listener.file_id)).collect();
        let table = folder.join(LISTENERS);
        write(&table, &listener_table(&inputs, &accepted, server.dispatcher_module().as_ref()))?;
        written.insert(table);
    }

    let (target, report) = ReportingTarget::buffer();
    Reporter::new(
        database.read_only(),
        ReporterConfig {
            target,
            format: ReportingFormat::Json,
            color_choice: ReportingColorChoice::Never,
            filter_fixable: false,
            sort: false,
            minimum_report_level: None,
            editor_url: None,
        },
    )
    .report(refused, None)?;
    let report = report.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let path = folder.join(HOST_REPORT);
    write(&path, &report)?;
    written.insert(path);
    let deleted = delete_all_but(folder, &written, &folder.join(PACKAGE_FOLDER))?;
    if let Some(record) = record {
        let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
        record.write(&record_path, String::new(), database, stamped.into_iter().chain(listed), written)?;
    }

    Ok(changed || deleted > 0)
}

/// Where the hosts' compile of the project at `root` into `folder` puts the compiled file of `source`, at its real path
/// relative to `root` with `.sharp` replaced by `.sharpc`, or none when `source` is a link outside `root`.
fn host_compiled_path(root: &Path, folder: &Path, source: &Path) -> Result<Option<PathBuf>, DatabaseError> {
    let real = source.canonicalize().map_err(|error| DatabaseError::io(IOAction::ResolvePath, source, error))?;

    Ok(real.strip_prefix(root).ok().map(|relative| folder.join(relative).with_extension("sharpc")))
}

/// The project's database: every file the analyzer loads with the prelude, and each `.sharp` file of a package as a
/// file of the project, which the compile checks too. Also gives the names of those packaged files.
pub(crate) fn project_database(
    orchestrator: &Orchestrator<'_>,
    workspace: &Path,
) -> Result<(Database<'static>, HashSet<Vec<u8>>), Error> {
    let mut database = orchestrator.load_database(workspace, true, None, None)?;
    database.merge_base(Prelude::decode_database(PRELUDE_BYTES).expect("Failed to decode embedded prelude"));
    let packaged: Vec<File> = database
        .files()
        .filter(|file| file.file_type == FileType::Vendored && Dialect::of(file).is_sharp())
        .map(|file| {
            let mut host = File::new(file.name.clone(), FileType::Host, file.path.clone(), file.contents.clone());
            host.is_standard_library = file.is_standard_library;
            host.folder_namespace = file.folder_namespace.clone();
            host.is_project = file.is_project;
            host
        })
        .collect();
    let included: HashSet<Vec<u8>> = packaged.iter().map(|file| file.name.to_vec()).collect();
    for file in packaged {
        database.add(file);
    }

    Ok((database.into_static(), included))
}

/// The PHP# files of `database`, which the compile lowers, as the scope of its analysis. The compile refuses a PHP#
/// file for the errors reported in it, and a node-analysis hook reports only in the file it inspects, so the hooks run
/// only in these files, as they run only in the named files of `mago analyze <paths>`. A scope that names no path is
/// the whole workspace, so a project with no PHP# file is scoped to every `.sharp` file, of which it has none. The glob
/// settings shape only excludes, and the scope has none, so it takes the default ones.
fn lowered_files(workspace: &Path, database: &impl DatabaseReader) -> Result<WorkspaceMatcher, Error> {
    let paths: Vec<Cow<'_, [u8]>> = database
        .files()
        .filter(|file| file.file_type == FileType::Host && Dialect::of(file).is_sharp())
        .map(|file| Cow::Owned(file.name.to_vec()))
        .collect();

    Ok(WorkspaceMatcher::from_configuration(&DatabaseConfiguration {
        workspace: Cow::Borrowed(workspace),
        paths,
        includes: Vec::new(),
        patches: Vec::new(),
        excludes: Vec::new(),
        extensions: vec![Cow::Borrowed(b"sharp".as_slice())],
        glob: GlobSettings::default(),
    })?)
}

/// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every other
/// file in the `.sharp` folder, as [`CompileCommand::execute`] says.
fn compile(configuration: &Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
    let workspace = configuration.source.workspace.as_path();
    let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.to_path_buf(), error))?;
    let compiled_folder = root.join(unit::COMPILED_FOLDER);
    let record_path = compiled_folder.join(INPUTS);
    let sources = source_files(configuration, &root);
    // A host names none of what it reads, so only a compile without hosts can know it has nothing to do.
    if !configuration.extension_hosts.values().any(|host| host.enabled)
        && let Some(record) = Record::read(&record_path)
        && every_input_is_current(&record, configuration, &root, &sources)
    {
        tracing::info!("{}", record.summary);

        return Ok(ExitCode::SUCCESS.into());
    }
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);
    let record = Record::start(configuration, &root, &sources);

    let mut orchestrator = create_orchestrator(configuration, color_choice, false, true, false);
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

    let (database, included) = project_database(&orchestrator, workspace)?;

    // Written before the compile stamps its inputs, which hold the PHP file a default class's name could name in
    // a file's folder, so a compiled file is current once it is written.
    let mut failed = false;
    let mut written_beside = BTreeSet::new();
    let mut folders = BTreeSet::new();
    let mut listings = HashMap::new();
    for file in database.files().filter(|file| {
        file.file_type == FileType::Host
            && Dialect::of(file).is_sharp()
            && !file.is_standard_library
            && !file.name.starts_with(VENDOR)
            && !included.contains(file.name.as_ref())
    }) {
        let source = root.join(String::from_utf8_lossy(&file.name).as_ref());
        let folder = source.parent().unwrap_or(&root).to_path_buf();
        folders.insert(folder.clone());
        if file.folder_namespace == Some(FolderNamespace::Test) {
            continue;
        }
        let Some(declared) = declared_type(&file) else {
            continue;
        };
        let (type_file, class) = (&declared.file, &declared.name);
        let php = format!("{}.php", type_file.strip_suffix(".sharp").unwrap_or(type_file));
        let listing = match listings.entry(folder.clone()) {
            Entry::Occupied(listing) => listing.into_mut(),
            Entry::Vacant(listing) => listing.insert(file_names(&folder)?),
        };
        // PHP names a class whatever its case, and a disk that ignores case opens `order.php` for `Order.php`.
        let named: Vec<&String> = listing.iter().filter(|name| name.eq_ignore_ascii_case(&php)).collect();
        let mut someone_wrote = None;
        for name in &named {
            if !written_by_compile(&folder.join(name))? {
                someone_wrote = Some(name);
                break;
            }
        }
        if let Some(name) = someone_wrote {
            let source = String::from_utf8_lossy(&file.name);
            let written = format!("{}{name}", &source[..source.len() - type_file.len()]);
            tracing::error!("{written} and {source} both name {class}. Delete {written}, or rename one of them.");
            failed = true;
            continue;
        }
        // A rename onto a file of another case keeps that file's name on a disk that ignores case.
        for name in named.into_iter().filter(|name| **name != php) {
            let path = folder.join(name);
            std::fs::remove_file(&path)
                .or_else(ignore_missing)
                .map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
        }
        let path = folder.join(&php);
        let contents = beside(&declared);
        if !listing.contains(&php)
            || std::fs::read(&path).map_err(|error| DatabaseError::io(IOAction::ReadFile, &path, error))?
                != contents.as_bytes()
        {
            write_by_rename(&path, contents.as_bytes())?;
        }
        written_beside.insert(path);
    }
    // A removed source's PHP file is in a folder that holds a source, or held one at the last compile.
    source_folders(&root, &compiled_folder, &compiled_folder, &mut folders)?;
    for folder in folders {
        for name in file_names(&folder)? {
            let path = folder.join(&name);
            if name.ends_with(".php") && !written_beside.contains(&path) && written_by_compile(&path)? {
                std::fs::remove_file(&path)
                    .or_else(ignore_missing)
                    .map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
            }
        }
    }
    git::exclude(&root, "mago compile", &written_beside.iter().cloned().collect::<Vec<_>>())?;

    let library = match standard_library(&root, &database) {
        Ok(library) => library,
        Err(refusal) => {
            tracing::error!("{refusal}");

            return Ok(ExitCode::FAILURE.into());
        }
    };
    let lowered = lowered_files(workspace, &database)?;
    let mut server =
        Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator)).scoped_to(lowered);
    server.analyze()?;
    // The compile hides what `mago analyze` hides: a host rule's error an `ignore` entry or the baseline targets.
    let ignored =
        CompiledIgnoreSet::compile(&configuration.analyzer.ignore, configuration.source.glob.to_database_settings());
    let baseline = match configuration.analyzer.baseline.as_deref().filter(|path| path.exists()) {
        Some(path) => Some(unserialize_baseline(path)?.0),
        None => None,
    };
    let read_database = server.database().read_only();
    let lean = Lean::new(&root);
    let mut stamped = Vec::new();
    let compilations = server.compile(
        |path| {
            let input = unit::stamp(&root, path)?;
            stamped.push(recorded(path, input.as_ref()));
            Ok(input)
        },
        |translations| lean.check(translations),
        &library,
        |mut issues| {
            issues.filter_out_ignored(&ignored, |file_id| {
                read_database.get_ref(&file_id).ok().map(|file| String::from_utf8_lossy(&file.name).into_owned())
            });
            match &baseline {
                Some(baseline) => baseline.filter_issues(issues, &read_database),
                None => issues,
            }
        },
    )?;
    let database = server.database();

    let mut written = BTreeSet::new();
    let mut accepted = HashSet::new();
    let mut refused = IssueCollection::default();
    let mut refused_files = 0;
    let mut refused_php_files = 0;
    let mut packaged_refusals = Vec::new();
    for (file_id, compilation) in compilations {
        let file = database.get_ref(&file_id)?;
        let name = String::from_utf8_lossy(&file.name);
        let source = file.path.clone().unwrap_or_else(|| root.join(name.as_ref()));
        let Some(compiled) = unit::compiled_path(&root, &source)? else {
            tracing::error!(
                "{name} is a link to {}, outside the project, so it was not compiled. Install the package as a copy instead of a link.",
                source
                    .canonicalize()
                    .map_err(|error| DatabaseError::io(IOAction::ResolvePath, &source, error))?
                    .display()
            );
            failed = true;
            continue;
        };

        match compilation {
            Compilation::Accepted(bytes) => {
                write_by_rename(&compiled, &bytes)?;
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Refused(errors) => {
                refused.extend(errors);
                if Dialect::of(file).is_sharp() {
                    refused_files += 1;
                } else {
                    refused_php_files += 1;
                }
                if let Some(package) = package_of(&file.name) {
                    packaged_refusals.push(format!(
                            "{name} comes from the Composer package {package}. Update the package, or report the error to its maintainers."
                        ));
                }
            }
        }
    }

    let listeners = server.listeners();
    let inputs = listener_inputs(&root, database)?;
    if !inputs.is_empty() {
        let accepted: Vec<&Listener> =
            listeners.iter().filter(|listener| accepted.contains(&listener.file_id)).collect();
        let table = compiled_folder.join(LISTENERS);
        write_by_rename(&table, &listener_table(&inputs, &accepted, server.dispatcher_module().as_ref()))?;
        written.insert(table);
    }
    if compiled_folder.is_dir() {
        delete_all_but(&compiled_folder, &written, &compiled_folder.join(PACKAGE_FOLDER))?;
    }

    for folder in package_compiled_folders(&root, database) {
        tracing::error!(
            "{folder} is a compiled folder inside a package, and the engine would read it instead of the project's .sharp folder. Delete {folder}."
        );
        failed = true;
    }

    if !refused.is_empty() {
        let reporter = Reporter::new(
            database.read_only(),
            ReporterConfig {
                target: Default::default(),
                format: default_reporting_format(),
                color_choice: match color_choice {
                    ColorChoice::Auto => ReportingColorChoice::Auto,
                    ColorChoice::Always => ReportingColorChoice::Always,
                    ColorChoice::Never => ReportingColorChoice::Never,
                },
                filter_fixable: false,
                sort: false,
                minimum_report_level: None,
                editor_url: configuration.editor_url.clone(),
            },
        );
        reporter.report(refused, None)?;
    }
    for refusal in packaged_refusals {
        tracing::error!("{refusal}");
    }

    let mut summary = match (accepted.len(), refused_files) {
        (0, 0) => "No PHP# files found to compile.".to_string(),
        (compiled, _) => format!("Compiled {} into .sharp/.", files(compiled)),
    };
    let mut errors = Vec::new();
    match refused_files {
        0 => {}
        1 => errors.push("1 PHP# file was refused".to_string()),
        refused => errors.push(format!("{refused} PHP# files were refused")),
    }
    match refused_php_files {
        0 => {}
        1 => errors.push("1 plain PHP file was refused".to_string()),
        refused => errors.push(format!("{refused} plain PHP files were refused")),
    }
    if !errors.is_empty() {
        summary.push_str(&format!(" {}: fix the errors and run vendor/bin/mago compile again.", errors.join(", and ")));
    }
    if failed || !errors.is_empty() {
        tracing::error!("{summary}");

        return Ok(ExitCode::FAILURE.into());
    }
    // A project with no PHP# file has no `.sharp` folder to keep the record in.
    if let Some(record) = record
        && !written.is_empty()
    {
        let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
        let outputs = written.into_iter().chain(written_beside);
        record.write(&record_path, summary.clone(), database, stamped.into_iter().chain(listed), outputs)?;
    }
    tracing::info!("{summary}");

    Ok(ExitCode::SUCCESS.into())
}

/// Whether the hosts' compile in `folder` of the project at `root` has nothing to do: the project has no PHP# file for
/// the hosts to load, or the record of the compile's last run stands for a compile with `configuration` now, whose
/// source files are `files`, and each PHP# file the compile checks has a compiled file there the engine runs, one whose
/// every input is as it was stamped, written by this checker build. A source that is a link outside the project never
/// has one. `stamp` stamps each input once, however many compiled files name it.
///
/// # Errors
///
/// Returns the error of reading a compiled file or of stamping one of its inputs.
fn every_compiled_file_is_current(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    files: &BTreeMap<String, Status>,
    database: &impl DatabaseReader,
    mut stamp: impl FnMut(&[u8]) -> Result<Option<unit::Input>, DatabaseError>,
) -> Result<bool, Error> {
    let sources: Vec<_> =
        database.files().filter(|file| file.file_type == FileType::Host && Dialect::of(file).is_sharp()).collect();
    if sources.is_empty() {
        return Ok(true);
    }
    if !Record::read(&folder.join(INPUTS))
        .is_some_and(|record| every_input_is_current(&record, configuration, root, files))
    {
        return Ok(false);
    }

    let mut stamps: HashMap<Vec<u8>, Option<unit::Input>> = HashMap::new();
    for file in sources {
        let source = file.path.clone().unwrap_or_else(|| root.join(String::from_utf8_lossy(&file.name).as_ref()));
        let Some(compiled) = host_compiled_path(root, folder, &source)? else {
            continue;
        };
        let bytes = match std::fs::read(&compiled) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &compiled, error).into()),
        };
        let stale = unit::is_stale(&bytes, |path| match stamps.entry(path.to_vec()) {
            Entry::Occupied(stamped) => Ok(stamped.get().clone()),
            Entry::Vacant(unstamped) => Ok(unstamped.insert(stamp(path)?).clone()),
        })?;
        if stale {
            return Ok(false);
        }
    }

    Ok(true)
}

/// What a compile that succeeded read and wrote, as [`INPUTS`] holds it: the header the compile ran under, the status of
/// each source file before the compile read it with the hash of the bytes it read, the stamp of each other input, and
/// the status of each file it wrote or kept once it was done.
#[derive(Serialize, Deserialize)]
struct Record {
    /// The build ID of the checker that wrote it.
    build: [u8; 16],
    /// The project's canonical root.
    root: PathBuf,
    /// The hash of the configuration, as [`configuration_hash`] gives it.
    configuration: [u8; 16],
    /// What the compile printed once it was done.
    summary: String,
    /// The filesystem's time when the compile started, in nanoseconds since the epoch.
    marker: i64,
    /// Each file [`source_files`] found other than an output, by name.
    files: BTreeMap<String, (Status, [u8; 16])>,
    /// Each other input, as [`recorded`] holds it.
    inputs: Vec<Stamp>,
    /// Each file the compile wrote or kept, by name.
    outputs: BTreeMap<String, Status>,
}

impl Record {
    /// The record at `path`, or none when no record the compile reads is there.
    fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Starts the record of a compile of the project at `root` with `configuration`, before the compile reads a file:
    /// takes the filesystem's time, keeps `files`, and stamps each input the loader and the compile read apart from the
    /// analysis. Those are the `composer.json` of each PHP# source's folder, or of the nearest folder above it that holds
    /// one, as the loader reads it, each absent one below it, Composer's `installed.json`, the baseline, and the `.sharp`
    /// folder of each folder that holds a package's PHP# file, absent unless the compile refuses it. Gives none when an
    /// input can't be stamped, and the compile then keeps no record.
    fn start(configuration: &Configuration, root: &Path, files: &BTreeMap<String, Status>) -> Option<Self> {
        let marker = marker(root);
        let stamp = |path: &[u8]| unit::stamp(root, path).ok().map(|input| recorded(path, input.as_ref()));
        let mut inputs = Vec::new();
        let mut folders = HashSet::new();
        for name in files.keys().filter(|name| name.ends_with(".sharp")) {
            for folder in root.join(name).ancestors().skip(1) {
                if !folders.insert(folder.to_path_buf()) {
                    break;
                }
                let manifest = folder.join("composer.json");
                inputs.push(stamp(source_name(root, &manifest).as_bytes())?);
                if !std::fs::metadata(&manifest).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
                    break;
                }
            }
            if name.starts_with("vendor/") {
                for folder in package_compiled_folders_of(name) {
                    inputs.push(stamp(folder.as_bytes())?);
                }
            }
        }
        inputs.push(stamp(b"vendor/composer/installed.json")?);
        if let Some(baseline) = &configuration.analyzer.baseline {
            inputs.push(stamp(baseline.as_os_str().as_encoded_bytes())?);
        }

        Some(Self {
            build: BUILD_ID.to_be_bytes(),
            root: root.to_path_buf(),
            configuration: configuration_hash(configuration),
            summary: String::new(),
            marker,
            files: files.iter().map(|(name, status)| (name.clone(), (*status, [0; 16]))).collect(),
            inputs,
            outputs: BTreeMap::new(),
        })
    }

    /// Writes the record to `path` once the compile succeeded and printed `summary`: hashes each source file's bytes as
    /// `database` read them, or as the file holds them when the database didn't read it, adds `inputs`, the stamps the
    /// compile took, and takes the status of each of `outputs`, the files it wrote or kept.
    ///
    /// # Errors
    ///
    /// Returns the error of reading a file or of writing the record.
    fn write(
        mut self,
        path: &Path,
        summary: String,
        database: &impl DatabaseReader,
        inputs: impl IntoIterator<Item = Stamp>,
        outputs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), Error> {
        for output in outputs {
            let metadata = std::fs::metadata(&output)
                .map_err(|error| DatabaseError::io(IOAction::ReadMetadata, &output, error))?;
            let name = source_name(&self.root, &output);
            self.files.remove(&name);
            self.outputs.insert(name, Status::of(&metadata));
        }
        let mut files = BTreeMap::new();
        for (name, (status, _)) in std::mem::take(&mut self.files) {
            let hash = match database.get_by_name(name.as_bytes()) {
                Ok(file) => unit::source_hash(&file.contents),
                Err(_) => match std::fs::read(self.root.join(&name)) {
                    Ok(bytes) => unit::source_hash(&bytes),
                    // Gone since the compile found it, and never read.
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(DatabaseError::io(IOAction::ReadFile, &self.root.join(&name), error).into());
                    }
                },
            };
            files.insert(name, (status, hash));
        }
        self.inputs.extend(inputs);
        self.inputs.retain(|(path, ..)| !files.contains_key(path));
        self.inputs.sort_unstable();
        self.inputs.dedup();
        self.files = files;
        self.summary = summary;

        Ok(write_by_rename(path, &serde_json::to_vec(&self)?)?)
    }
}

/// Whether `record` stands for a compile of the project at `root` with `configuration` now, whose source files are
/// `files`: its header names this checker build, `root` and `configuration`, it holds the same source files, each
/// output's status is unchanged, each other input passes `unit::stamp`, and each source file's status is unchanged with
/// times older than the record's marker, or its bytes hash as the compile's did. A status that changed only makes the
/// file's bytes hashed, as git hashes a racily clean file of its index, so a write in the same tick as the compile's read
/// is caught. A record that can't be checked is not current, and the full compile reports what fails.
fn every_input_is_current(
    record: &Record,
    configuration: &Configuration,
    root: &Path,
    files: &BTreeMap<String, Status>,
) -> bool {
    let sources: Vec<(&String, &Status)> =
        files.iter().filter(|(name, _)| !record.outputs.contains_key(name.as_str())).collect();

    record.build == BUILD_ID.to_be_bytes()
        && record.root == root
        && record.configuration == configuration_hash(configuration)
        && sources.len() == record.files.len()
        && sources.iter().zip(&record.files).all(|((name, _), (recorded, _))| *name == recorded)
        && record.outputs.iter().all(|(name, status)| {
            std::fs::metadata(root.join(name)).is_ok_and(|metadata| Status::of(&metadata) == *status)
        })
        && record.inputs.iter().all(|(path, size, hash)| match unit::stamp(root, path.as_bytes()) {
            Ok(Some(input)) => (input.size, input.hash) == (*size, *hash),
            // On a disk that ignores case, the loader reads a file whose name differs only in case.
            Ok(None) => (*size, *hash) == (0, [0; 16]) && !root.join(path).exists(),
            Err(_) => false,
        })
        && sources.iter().zip(record.files.values()).all(|((name, status), (recorded, hash))| {
            (*status == recorded && status.is_older_than(record.marker))
                || std::fs::read(root.join(name)).is_ok_and(|bytes| unit::source_hash(&bytes) == *hash)
        })
}

/// An input as [`Record`] holds it: its path, size and hash, both zero when no file is there.
type Stamp = (String, u64, [u8; 16]);

/// The stamp of the input at `path`, as `unit::stamp` gave it.
fn recorded(path: &[u8], input: Option<&unit::Input>) -> Stamp {
    match input {
        Some(input) => (String::from_utf8_lossy(&input.path).into_owned(), input.size, input.hash),
        None => (String::from_utf8_lossy(path).into_owned(), 0, [0; 16]),
    }
}

/// A file's size, modification time, status change time, inode and device, each time in nanoseconds since the epoch.
/// Every write changes the status change time, a time `touch` sets back included, so a file whose status is unchanged
/// and whose times are older than a compile's start holds the bytes the compile read.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Status(u64, i64, i64, u64, u64);

impl Status {
    #[cfg(unix)]
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        let nanoseconds = |seconds: i64, fraction: i64| seconds.saturating_mul(1_000_000_000).saturating_add(fraction);

        Self(
            metadata.size(),
            nanoseconds(metadata.mtime(), metadata.mtime_nsec()),
            nanoseconds(metadata.ctime(), metadata.ctime_nsec()),
            metadata.ino(),
            metadata.dev(),
        )
    }

    /// With no status change time to read, a file's times are never older than a compile's start, so its bytes are
    /// always hashed.
    #[cfg(not(unix))]
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self(metadata.len(), 0, i64::MAX, 0, 0)
    }

    /// Whether both the file's times are older than `marker`.
    fn is_older_than(self, marker: i64) -> bool {
        self.1 < marker && self.2 < marker
    }
}

/// The filesystem's time now, as the modification time of a file it creates in `root`, which holds the files a compile
/// reads, or the earliest time when it can't create one there, so no file is older.
fn marker(root: &Path) -> i64 {
    tempfile::tempfile_in(root).and_then(|file| file.metadata()).map_or(i64::MIN, |metadata| Status::of(&metadata).1)
}

/// The hash of `configuration`, from its debug form, which holds every field, the ones serde skips included: the
/// deprecated `perform-heuristic-checks` and the formatter's settings.
fn configuration_hash(configuration: &Configuration) -> [u8; 16] {
    unit::source_hash(format!("{configuration:?}").as_bytes())
}

/// Each file the loader could read under the configured `paths`, `includes` and `patches` roots, by the name the
/// database gives it, with its status: a file a root names, whatever its extension, and each file of a configured
/// extension under a root folder or under the folder a glob pattern starts with, following links as the loader does.
/// The excludes are ignored, which only adds files, and the `.sharp` folder at `root` is left out, as the loader leaves
/// it out.
fn source_files(configuration: &Configuration, root: &Path) -> BTreeMap<String, Status> {
    let source = &configuration.source;
    let whole_workspace = [".".to_owned()];
    let paths = if source.paths.is_empty() { &whole_workspace[..] } else { &source.paths[..] };
    let is_glob = |text: &str| text.contains(['*', '?', '[', '{']);
    let mut files = BTreeMap::new();
    for configured in paths.iter().chain(&source.includes).chain(&source.patches) {
        let path = root.join(configured);
        if path.exists() {
            let path = path.canonicalize().unwrap_or(path);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    files.insert(source_name(root, &path), Status::of(&metadata));
                }
                Ok(_) => walk(root, &source.extensions, &path, &mut vec![path.clone()], false, &mut files),
                Err(_) => {}
            }
        } else if is_glob(configured) {
            let start: PathBuf =
                path.components().take_while(|part| !is_glob(&part.as_os_str().to_string_lossy())).collect();
            let start = start.canonicalize().unwrap_or(start);
            walk(root, &source.extensions, &start, &mut vec![start.clone()], true, &mut files);
        }
    }

    files
}

/// Adds to `files` each file of one of `extensions` under `folder`, with its status, by its name, or by the name of its
/// real path when `by_real_path` is set, as the loader names a file a glob pattern matches. `real` holds the real path
/// of `folder` and of each folder above it in the walk, and a folder whose real path it holds is a link back to one of
/// them, which the walk skips as the loader does.
fn walk(
    root: &Path,
    extensions: &[String],
    folder: &Path,
    real: &mut Vec<PathBuf>,
    by_real_path: bool,
    files: &mut BTreeMap<String, Status>,
) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // A link whose target is gone is no file, as the loader skips it.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let real_path = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => path.canonicalize().unwrap_or_else(|_| path.clone()),
                _ => real.last().map_or_else(|| path.clone(), |parent| parent.join(entry.file_name())),
            };
            if path != root.join(unit::COMPILED_FOLDER) && !real.contains(&real_path) {
                real.push(real_path);
                walk(root, extensions, &path, real, by_real_path, files);
                real.pop();
            }
        } else if path.extension().is_some_and(|extension| extensions.iter().any(|known| extension == known.as_str())) {
            let named = if by_real_path { path.canonicalize().unwrap_or(path) } else { path };
            files.insert(source_name(root, &named), Status::of(&metadata));
        }
    }
}

/// The name the database gives the file at `path`: its path relative to `root`, or its whole path outside `root`.
fn source_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned()
}

/// The standard library's package name, as its `composer.json` and Composer's `installed.json` give it.
const STANDARD_LIBRARY_PACKAGE: &str = "heyjordanparker/php-sharp-composer";

/// The standard library the project's files compile against: the native fingerprint its package's `native.php` gives,
/// which the engine compares with its own, and the version the `installed.json` of the `vendor/` folder that holds the
/// package lists, which the engine's refusal names, or none outside `vendor/`. A project without the standard library,
/// or whose library declares no native bodies in a `native.php`, compiles against none, an all-zero fingerprint.
///
/// Fails when the package's `native.php` gives no fingerprint, so no compiled file names a library it was not checked
/// against.
pub(crate) fn standard_library(root: &Path, database: &impl DatabaseReader) -> Result<unit::Library, String> {
    let Some(file) = database.files().find(|file| file.is_standard_library) else {
        return Ok(unit::Library::default());
    };
    let source = file.path.clone().unwrap_or_else(|| root.join(String::from_utf8_lossy(&file.name).as_ref()));
    let Some(package) = source.ancestors().skip(1).find(|folder| folder.join("composer.json").is_file()) else {
        return Err(format!(
            "{} belongs to no Composer package. Reinstall {STANDARD_LIBRARY_PACKAGE}.",
            source.display()
        ));
    };

    let native = package.join("native.php");
    if !native.exists() {
        return Ok(unit::Library::default());
    }
    let fingerprint = std::fs::read_to_string(&native).ok().and_then(|code| {
        let hex = code.split_once("'fingerprint' => '")?.1.get(..32)?;
        let mut fingerprint = [0; 16];
        for (byte, pair) in fingerprint.iter_mut().zip(hex.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }

        Some(fingerprint)
    });
    let Some(native_fingerprint) = fingerprint else {
        return Err(format!("{} gives no native fingerprint. Reinstall {STANDARD_LIBRARY_PACKAGE}.", native.display()));
    };

    let installed = package.parent().and_then(Path::parent).map(|vendor| vendor.join("composer/installed.json"));
    let version = installed
        .and_then(|installed| std::fs::read(installed).ok())
        .and_then(|installed| serde_json::from_slice::<serde_json::Value>(&installed).ok())
        .and_then(|installed| {
            let packages = installed.get("packages").unwrap_or(&installed).as_array()?.clone();
            packages
                .into_iter()
                .find(|entry| entry.get("name").and_then(serde_json::Value::as_str) == Some(STANDARD_LIBRARY_PACKAGE))
        })
        .and_then(|entry| entry.get("version").and_then(serde_json::Value::as_str).map(str::to_owned))
        .unwrap_or_default();

    Ok(unit::Library { native_fingerprint, version: version.into_bytes() })
}

/// The inputs of the listener table, stamped as the engine reads them, which it checks only while it compiles on
/// demand: every PHP# file, which an `on` clause or a module binding the dispatcher may join, and the listing of the
/// folder of each and of each folder above it, which a PHP# file or a namespace folder joining or leaving changes. A
/// project with no PHP# file has none.
fn listener_inputs(root: &Path, database: &impl DatabaseReader) -> Result<Vec<unit::Input>, Error> {
    let mut paths = BTreeSet::new();
    for file in database.files().filter(|file| file.file_type == FileType::Host && Dialect::of(file).is_sharp()) {
        paths.insert(file.name.to_vec());
        let mut folder = &file.name[..];
        while let Some(slash) = folder[..folder.len().saturating_sub(1)].iter().rposition(|byte| *byte == b'/') {
            folder = &folder[..=slash];
            paths.insert([folder, unit::SOURCE_LISTING.as_bytes()].concat());
        }
        paths.insert(unit::SOURCE_LISTING.as_bytes().to_vec());
    }

    let mut inputs = Vec::with_capacity(paths.len());
    for path in paths {
        inputs.extend(unit::stamp(root, &path)?);
    }

    Ok(inputs)
}

/// The PHP source of the listener table. Its `inputs` are the files whose change makes the table out of date, each as
/// its workspace-relative path, size, modification time and hash. Its `listeners` are one row per listener, in the
/// order the engine runs them, of its class, its method, the events its `on` clause lists, and whether it takes the
/// event. A full name holds no `'` and never ends in `\`, so each one is a single-quoted string as written. Its
/// `dispatcher` is the app's module whose binding of the dispatcher serves the whole app, as its path and namespace,
/// or `null`.
fn listener_table(inputs: &[unit::Input], listeners: &[&Listener], dispatcher: Option<&DispatcherModule>) -> Vec<u8> {
    let mut table = b"<?php\n\nreturn [\n    'inputs' => [\n".to_vec();
    for input in inputs {
        let hash: String = input.hash.iter().map(|byte| format!("{byte:02x}")).collect();
        table.extend_from_slice(b"        [");
        quoted(&mut table, &input.path);
        table.extend_from_slice(format!(", {}, {}, '{hash}'],\n", input.size, input.mtime_ns).as_bytes());
    }
    table.extend_from_slice(b"    ],\n    'listeners' => [\n");
    for listener in listeners {
        let events: Vec<String> = listener.events.iter().map(|event| format!("'{event}'")).collect();
        table.extend_from_slice(
            format!(
                "        ['{}', '{}', [{}], {}],\n",
                listener.class,
                listener.method,
                events.join(", "),
                listener.takes_event
            )
            .as_bytes(),
        );
    }
    table.extend_from_slice(b"    ],\n    'dispatcher' => ");
    match dispatcher {
        Some(module) => {
            table.push(b'[');
            quoted(&mut table, module.file.as_bytes());
            table.extend_from_slice(b", ");
            quoted(&mut table, module.namespace.as_bytes());
            table.extend_from_slice(b"],\n");
        }
        None => table.extend_from_slice(b"null,\n"),
    }
    table.extend_from_slice(b"];\n");

    table
}

/// Appends `text` to `table` as a PHP single-quoted string.
fn quoted(table: &mut Vec<u8>, text: &[u8]) {
    table.push(b'\'');
    for byte in text {
        if matches!(byte, b'\'' | b'\\') {
            table.push(b'\\');
        }
        table.push(*byte);
    }
    table.push(b'\'');
}

/// The PHP file beside the PHP# source `declared`, spec section 27, in `mago fmt`'s layout. PHP code that lists `*.php`
/// files and names each class by its path finds the PHP# class through it, and Composer's PSR-4 loads it before the
/// source. It loads the source through the engine, and on plain PHP it throws before any source can reach the output.
/// Its last statement states the type's declaration and never runs, so a tool that reads PHP source, as Composer's
/// class map and Symfony's attribute routing do, finds the type in it, and the engine declares the type once, from the
/// source. PHP refuses a keyword as a type's name even in a declaration that never runs, so the file of a type named
/// after one, as `Case`, states none and parses. Once the source is gone it declares nothing, as if it were not there,
/// until the next compile deletes it.
fn beside(declared: &DeclaredType) -> String {
    let DeclaredType { file, keyword, .. } = declared;
    let name = file.strip_suffix(".sharp").unwrap_or(file);
    let (finds, declaration) = match keyword {
        Some(keyword) => (
            "lists or reads",
            format!(
                "\n// The engine declares {name} from {file}, and this declaration never runs.\nif (false) {{\n    {keyword} {name}\n    {{\n    }}\n}}\n"
            ),
        ),
        None => ("lists", String::new()),
    };
    let namespace = match declared.name.rsplit_once('.') {
        Some((namespace, _)) => format!("namespace {};\n\n", namespace.replace('.', "\\")),
        None => String::new(),
    };

    format!(
        "{COMPANION_START}{file}, so PHP code that {finds} *.php files finds its type.\n\ndeclare(strict_types=1);\n\n{namespace}if (!\\is_file(__DIR__ . '/{file}')) {{\n    return;\n}}\n\n\\function_exists('Sharp\\Internal\\requireNative')\n    || throw new \\Error(\n        'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',\n    );\n\nrequire_once __DIR__ . '/{file}';\n{declaration}"
    )
}

/// Whether the file at `path` is one `mago compile` wrote beside a source, which starts with [`COMPANION_START`].
fn written_by_compile(path: &Path) -> Result<bool, DatabaseError> {
    let mut start = Vec::with_capacity(COMPANION_START.len());
    let read =
        std::fs::File::open(path).and_then(|file| file.take(COMPANION_START.len() as u64).read_to_end(&mut start));
    match read {
        Ok(_) => Ok(start == COMPANION_START.as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DatabaseError::io(IOAction::ReadFile, path, error)),
    }
}

/// The type a PHP# source declares named like its file, spec section 27.
struct DeclaredType {
    /// The source's file name, as `Order.sharp`.
    file: String,
    /// The type's PHP# name, as `App.Billing.Order`.
    name: String,
    /// The PHP keyword of the type the engine declares: `interface`, `enum` for an enum that runs as a PHP enum, or
    /// `class` for a class, a struct, an event, and an enum whose cases are classes. It is none for a type PHP source
    /// can't declare, one named after a PHP keyword.
    keyword: Option<&'static str>,
}

/// The type the PHP# source `file` declares, when it declares one named like it, spec section 27: a class, interface,
/// enum, struct or event. A module's file and an `extern` declaration file declare none.
fn declared_type(file: &File) -> Option<DeclaredType> {
    let name = String::from_utf8_lossy(&file.name);
    let type_file = name.rsplit('/').next()?;
    let stem = type_file.strip_suffix(".sharp")?;
    let arena = LocalArena::new();
    let program = parse_file(&arena, file);
    let namespace = program.statements.iter().find_map(|statement| match statement {
        Statement::Namespace(namespace) => Some(namespace),
        _ => None,
    });
    let namespaced = namespace.map(|namespace| namespace.statements().iter());
    let keyword =
        program.statements.iter().chain(namespaced.into_iter().flatten()).find_map(|statement| match statement {
            Statement::Class(class) if class.name.value == stem.as_bytes() => Some("class"),
            Statement::Struct(r#struct) if r#struct.name.value == stem.as_bytes() => Some("class"),
            Statement::Enum(r#enum) if r#enum.name.value == stem.as_bytes() => {
                Some(if r#enum.runs_as_classes() { "class" } else { "enum" })
            }
            Statement::Interface(interface) if interface.name.value == stem.as_bytes() => Some("interface"),
            Statement::Event(event) if event.name.value == stem.as_bytes() => Some("class"),
            _ => None,
        })?;
    let name = match namespace.and_then(|namespace| namespace.name.as_ref()) {
        Some(namespace) => format!("{}.{stem}", String::from_utf8_lossy(namespace.value()).replace('\\', ".")),
        None => stem.to_string(),
    };

    let keyword = (!is_php_keyword(stem.as_bytes())).then_some(keyword);

    Some(DeclaredType { file: type_file.to_string(), name, keyword })
}

/// The names of the files in `folder`, exactly as the folder holds them, or none when it is gone.
fn file_names(folder: &Path) -> Result<Vec<String>, DatabaseError> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DatabaseError::io(IOAction::OpenFolder, folder, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?;
        let file_type =
            entry.file_type().map_err(|error| DatabaseError::io(IOAction::ReadMetadata, &entry.path(), error))?;
        if file_type.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }

    Ok(names)
}

/// Adds to `folders` the folder of the source of each compiled file under `folder` of the compiled folder `compiled`,
/// outside `vendor/` and the folder the Lean step owns.
fn source_folders(
    root: &Path,
    compiled: &Path,
    folder: &Path,
    folders: &mut BTreeSet<PathBuf>,
) -> Result<(), DatabaseError> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(DatabaseError::io(IOAction::OpenFolder, folder, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?;
        let path = entry.path();
        let relative = path.strip_prefix(compiled).unwrap_or(&path);
        if entry.file_type().map_err(|error| DatabaseError::io(IOAction::ReadMetadata, &path, error))?.is_dir() {
            if relative != Path::new(PACKAGE_FOLDER) && relative != Path::new("vendor") {
                source_folders(root, compiled, &path, folders)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "sharpc") {
            folders.insert(root.join(relative.parent().unwrap_or(Path::new(""))));
        }
    }

    Ok(())
}

/// Treats a file that is already gone as removed.
fn ignore_missing(error: std::io::Error) -> std::io::Result<()> {
    if error.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(error) }
}

/// `count` PHP# files, in words.
fn files(count: usize) -> String {
    if count == 1 { "1 PHP# file".to_string() } else { format!("{count} PHP# files") }
}

/// Deletes every file under `folder` that is not in `kept` and not under the folder `owned`, which the Lean step owns,
/// then every folder left empty, `folder` included. Returns how many files it deleted.
fn delete_all_but(folder: &Path, kept: &BTreeSet<PathBuf>, owned: &Path) -> Result<usize, DatabaseError> {
    let open = |error| DatabaseError::io(IOAction::OpenFolder, folder, error);
    let mut deleted = 0;
    for entry in std::fs::read_dir(folder).map_err(open)? {
        let entry = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?;
        let path = entry.path();
        if path == owned || kept.contains(&path) {
            continue;
        }
        if entry.file_type().map_err(|error| DatabaseError::io(IOAction::ReadMetadata, &path, error))?.is_dir() {
            deleted += delete_all_but(&path, kept, owned)?;
        } else {
            std::fs::remove_file(&path).map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
            deleted += 1;
        }
    }
    if std::fs::read_dir(folder).map_err(open)?.next().is_none() {
        std::fs::remove_dir(folder).map_err(|error| DatabaseError::io(IOAction::DeleteFolder, folder, error))?;
    }

    Ok(deleted)
}

/// The Composer package of the file named `name`, as `vendor/package`, or none outside `vendor/`.
fn package_of(name: &[u8]) -> Option<String> {
    let mut parts = name.strip_prefix(VENDOR)?.splitn(3, |byte| *byte == b'/');
    let (vendor, package, _) = (parts.next()?, parts.next()?, parts.next()?);

    Some(format!("{}/{}", String::from_utf8_lossy(vendor), String::from_utf8_lossy(package)))
}

/// The workspace-relative name of each `.sharp` folder in a folder that holds a loaded PHP# file of `vendor/`, with `/`
/// between its parts as in every file name the database holds.
fn package_compiled_folders(root: &Path, database: &impl DatabaseReader) -> BTreeSet<String> {
    database
        .files()
        .filter(|file| file.name.starts_with(VENDOR) && Dialect::of(file).is_sharp())
        .flat_map(|file| package_compiled_folders_of(&String::from_utf8_lossy(&file.name)).collect::<Vec<_>>())
        .filter(|compiled| root.join(compiled).is_dir())
        .collect()
}

/// The workspace-relative name of the `.sharp` folder of each folder that holds the package file `name`, up to
/// `vendor/`.
fn package_compiled_folders_of(name: &str) -> impl Iterator<Item = String> {
    let mut folder = name;
    std::iter::from_fn(move || {
        let (parent, _) = folder.rsplit_once('/')?;
        folder = parent;

        Some(format!("{parent}/{}", unit::COMPILED_FOLDER))
    })
}

#[cfg(test)]
mod tests {
    use mago_php_version::PHPVersion;

    use super::*;
    use crate::config::CliOverrides;
    use crate::version_check::VersionDriftFailLevel;

    fn write(root: &Path, name: &str, contents: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// `Acme\Rate` in `vendor/`, whose `percent()` returns `returned`.
    fn rate(returned: &str) -> String {
        format!(
            "<?php\n\nnamespace Acme;\n\nfinal class Rate\n{{\n    public static function percent(): {returned}\n    {{\n        return 5;\n    }}\n}}\n"
        )
    }

    /// Each input the hosts' compile stamps, which the compiled files of `app/Order.sharp` and `app/Paced.sharp` share,
    /// as `composer.lock`, is stamped once. A `vendor/` file a PHP# file calls is no input of its compiled file, which
    /// `composer.lock` stands for, so the record of the compile's source files is what sees an edit of it.
    #[test]
    fn the_hosts_check_stamps_each_input_once_and_a_vendor_edit_compiles_again() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        write(&root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
        write(&root, "composer.lock", "{}\n");
        write(&root, "vendor/acme/rate/src/Rate.php", &rate("int"));
        write(&root, "app/Order.sharp", "namespace App;\n\npublic class Order\n{\n    public int total() => 1;\n}\n");
        write(
            &root,
            "app/Paced.sharp",
            "namespace App;\n\nimport Acme.Rate;\n\npublic class Paced\n{\n    public int percent() => Rate.percent();\n}\n",
        );
        let configuration = Configuration::load(CliOverrides {
            workspace: Some(root.clone()),
            config: Some(root.join("mago.toml")),
            no_version_check: true,
            ..Default::default()
        })
        .unwrap();
        let folder = tempfile::tempdir().unwrap();
        assert!(compile_into(&configuration, &root, folder.path()).unwrap(), "a fresh folder is written");

        let mut orchestrator = create_orchestrator(&configuration, ColorChoice::Never, false, false, false);
        analyze_the_whole_workspace_without_paths(&mut orchestrator);
        let (database, _) = project_database(&orchestrator, &configuration.source.workspace).unwrap();
        let mut stamped: BTreeMap<String, usize> = BTreeMap::new();
        let current = every_compiled_file_is_current(
            &configuration,
            &root,
            folder.path(),
            &source_files(&configuration, &root),
            &database,
            |path| {
                *stamped.entry(String::from_utf8_lossy(path).into_owned()).or_default() += 1;
                unit::stamp(&root, path)
            },
        )
        .unwrap();

        assert!(current);
        assert!(stamped.contains_key("composer.lock"), "{stamped:?}");
        assert!(stamped.values().all(|count| *count == 1), "{stamped:?}");

        write(&root, "vendor/acme/rate/src/Rate.php", &rate("string"));
        let paced = std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap();
        assert!(compile_into(&configuration, &root, folder.path()).unwrap(), "the vendor edit compiles again");
        assert_ne!(std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap(), paced);
    }

    /// Every field of the configuration enters the record's header, the ones serde skips included, so a change to any
    /// of them makes the next compile run.
    #[test]
    fn each_configuration_field_changes_the_configuration_hash() {
        let base = Configuration::from_workspace(PathBuf::from("/project"));
        type Change = fn(&mut Configuration);
        let changes: [(&str, Change); 17] = [
            ("version", |changed| changed.version = Some("0.6".to_owned())),
            ("threads", |changed| changed.threads += 1),
            ("stack-size", |changed| changed.stack_size += 1),
            ("php-version", |changed| changed.php_version = PHPVersion::PHP80),
            ("allow-unsupported-php-version", |changed| changed.allow_unsupported_php_version ^= true),
            ("no-version-check", |changed| changed.no_version_check ^= true),
            ("version-drift-fail-level", |changed| changed.version_drift_fail_level = VersionDriftFailLevel::Patch),
            ("source", |changed| changed.source.excludes.push("legacy".to_owned())),
            ("extension-hosts", |changed| {
                changed.extension_hosts.insert("acme".to_owned(), ExtensionHostConfiguration::default());
            }),
            ("linter", |changed| changed.linter.excludes.push("legacy".to_owned())),
            ("parser", |changed| changed.parser.enable_short_tags ^= true),
            ("formatter", |changed| changed.formatter.settings.print_width += 1),
            ("analyzer", |changed| changed.analyzer.find_unused_definitions ^= true),
            ("analyzer.perform-heuristic-checks", |changed| changed.analyzer.perform_heuristic_checks = Some(false)),
            ("guard", |changed| changed.guard.excludes.push("legacy".to_owned())),
            ("editor-url", |changed| changed.editor_url = Some("phpstorm://open?file=%file%".to_owned())),
            ("config-file", |changed| changed.config_file = Some(PathBuf::from("/project/mago.toml"))),
        ];

        for (field, change) in changes {
            let mut changed = base.clone();
            change(&mut changed);
            assert_ne!(configuration_hash(&changed), configuration_hash(&base), "{field}");
        }
    }
}
