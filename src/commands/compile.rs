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
//! source loses its PHP file, and a `.gitignore` the compile writes in each folder that holds such files keeps them
//! out of git in that checkout alone.
//!
//! A compile that succeeds with no warning keeps a record of everything it read and wrote in the `.sharp` folder, and
//! the next compile checks it before any analysis: when every input and output is as it was, it prints the same summary
//! and does nothing else. A project with an extension host enabled compiles every time, since a host names none of what
//! it reads.

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
use mago_database::loader::ExcludedFolders;
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
use mago_syntax_core::utils::is_part_of_identifier;

use crate::baseline::unserialize_baseline;
use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::args::reporting::default_reporting_format;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::config::extension::ExtensionHostConfiguration;
use crate::consts::PRELUDE_BYTES;
use crate::consts::VERSION;
use crate::error::Error;
use crate::extensions::start_external_analyzer;
use crate::utils::create_orchestrator;
use crate::utils::git;
use crate::utils::logger;
use crate::utils::rename_into;
use crate::utils::temp_file_in;
use crate::utils::write_by_rename;

/// The folder Composer installs packages into.
const VENDOR: &[u8] = b"vendor/";

/// The listener table in the `.sharp` folder, which the engine reads to run each PHP# listener, spec section 15. Every
/// compile of a project with a PHP# file writes it, listeners or none, so the engine can tell when it is out of date.
const LISTENERS: &str = "listeners.php";

/// The record of everything the last compile that succeeded read and wrote, in the `.sharp` folder and in the hosts'
/// folder, which the next compile checks before any analysis: when every input and output is as it was, the compile has
/// nothing to do. A refused, failed, warned or interrupted compile leaves none, and so does one whose output changed
/// while it ran.
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
    let mut filesystems = Filesystems::new();
    let sources = source_files(configuration, root, &mut filesystems);
    if every_compiled_file_is_current(configuration, root, folder, &sources, &mut filesystems) {
        return Ok(false);
    }
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);
    let mut record = Record::start(configuration, root, &sources, folder, filesystems);
    let mut orchestrator = create_orchestrator(configuration, ColorChoice::Never, false, false, false);
    analyze_the_whole_workspace_without_paths(&mut orchestrator);
    orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
    let (database, _) = project_database(&orchestrator, &configuration.source.workspace)?;

    let library = standard_library(root, &database).map_err(OrchestratorError::General)?;
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
        if let Some(record) = record.as_mut() {
            record.output(path, bytes);
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

    let lowered = refused.is_empty();
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
    // A compile with a file that doesn't lower keeps no record, so the next one tries that file again.
    let record = record.filter(|_| lowered);
    written.extend(record.as_ref().and_then(Record::marker_path));
    let deleted = delete_all_but(folder, &written, &folder.join(PACKAGE_FOLDER))?;
    if let Some(record) = record {
        let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
        record.write(&record_path, String::new(), database, stamped.into_iter().chain(listed))?;
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
    let mut filesystems = Filesystems::new();
    let sources = source_files(configuration, &root, &mut filesystems);
    // A host names none of what it reads, so only a compile without hosts can know it has nothing to do.
    if !configuration.extension_hosts.values().any(|host| host.enabled)
        && let Some(record) = Record::read(&record_path)
        && every_input_is_current(&record, configuration, &root, &sources, &mut filesystems)
    {
        tracing::info!("{}", record.summary);

        return Ok(ExitCode::SUCCESS.into());
    }
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);
    // A project with no PHP# file gets no `.sharp` folder to keep a record in.
    let mut record = sources
        .keys()
        .any(|name| name.ends_with(".sharp"))
        .then(|| Record::start(configuration, &root, &sources, &compiled_folder, filesystems))
        .flatten();

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
    let mut written_beside: HashMap<PathBuf, BTreeSet<String>> = HashMap::new();
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
        if let Some(record) = record.as_mut() {
            record.output(&path, contents.as_bytes());
        }
        written_beside.entry(folder).or_default().insert(php);
    }
    // A removed source's PHP file, and the `.gitignore` that lists it, are in a folder that holds a source, held one at
    // the last compile, or holds the compile's `.gitignore`, as the folder of a refused source does.
    source_folders(&root, &compiled_folder, &compiled_folder, &mut folders)?;
    gitignore_folders(&root, &orchestrator.database_configuration(workspace, false), &mut folders)?;
    for folder in folders {
        let written = written_beside.remove(&folder).unwrap_or_default();
        for name in file_names(&folder)? {
            let path = folder.join(&name);
            if name.ends_with(".php") && !written.contains(&name) && written_by_compile(&path)? {
                std::fs::remove_file(&path)
                    .or_else(ignore_missing)
                    .map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
            }
        }
        keep_out_of_git(&root, &folder, &written)?;
        // The `.gitignore` of a folder of PHP files is the compile's, which it wrote, or someone else's, which it reads.
        if let Some(record) = record.as_mut().filter(|_| !written.is_empty()) {
            let gitignore = folder.join(".gitignore");
            match std::fs::read(&gitignore) {
                Ok(contents) if gitignore_written_by_compile(&contents) => record.output(&gitignore, &contents),
                _ => record.input(&gitignore),
            }
        }
    }
    git::remove_exclude_blocks(&root, "mago compile")?;

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
                if let Some(record) = record.as_mut() {
                    record.output(&compiled, &bytes);
                }
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
        let bytes = listener_table(&inputs, &accepted, server.dispatcher_module().as_ref());
        write_by_rename(&table, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&table, &bytes);
        }
        written.insert(table);
    }
    // A compile that writes no compiled file leaves no `.sharp` folder to keep a record in.
    let record = record.filter(|_| !written.is_empty());
    if compiled_folder.is_dir() {
        let kept: BTreeSet<PathBuf> =
            written.iter().cloned().chain(record.as_ref().and_then(Record::marker_path)).collect();
        delete_all_but(&compiled_folder, &kept, &compiled_folder.join(PACKAGE_FOLDER))?;
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
    if let Some(record) = record {
        let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
        record.write(&record_path, summary.clone(), database, stamped.into_iter().chain(listed))?;
    }
    tracing::info!("{summary}");

    Ok(ExitCode::SUCCESS.into())
}

/// Whether the hosts' compile in `folder` of the project at `root` has nothing to do: the project has no PHP# file for
/// the hosts to load, or the record of the compile's last run in `folder` stands for a compile with `configuration` now,
/// whose source files are `sources`, as [`every_input_is_current`] checks it.
fn every_compiled_file_is_current(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    sources: &BTreeMap<String, Found>,
    filesystems: &mut Filesystems,
) -> bool {
    !sources.keys().any(|name| name.ends_with(".sharp"))
        || Record::read(&folder.join(INPUTS))
            .is_some_and(|record| every_input_is_current(&record, configuration, root, sources, filesystems))
}

/// What a compile that succeeded read and wrote, as [`INPUTS`] holds it: the header the compile ran under, each source
/// file as the compile found it before it read it, with the hash of the bytes it read, the stamp of each other input,
/// and the status of each file it wrote or kept right after, with the hash of its bytes.
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
    /// The modification time of the record's own temp file, which the compile created when it started, in nanoseconds
    /// since the epoch, as the filesystem's clock gave it.
    marker: i64,
    /// Each file [`source_files`] found other than an output, by name.
    files: BTreeMap<String, (Found, [u8; 16])>,
    /// Each other input, as [`recorded`] holds it.
    inputs: Vec<Stamp>,
    /// Each file the compile wrote or kept, by name.
    outputs: BTreeMap<String, (Status, [u8; 16])>,
    /// The record's own temp file, whose modification time is the marker, which the compile keeps until it is done.
    #[serde(skip)]
    marker_file: Option<tempfile::NamedTempFile>,
    /// How many warnings the logger had printed when the compile started.
    #[serde(skip)]
    warnings: usize,
    /// Whether an output's status or an input's stamp couldn't be taken, so the record can't stand for the compile.
    #[serde(skip)]
    lost: bool,
    /// Whether each filesystem's change time records writes, for the outputs' statuses.
    #[serde(skip)]
    filesystems: Filesystems,
}

impl Record {
    /// The record at `path`, or none when no record the compile reads is there.
    fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Starts the record of a compile of the project at `root` with `configuration` in `folder`, before the compile
    /// reads a file: creates the record's own temp file there and takes its modification time as the marker, keeps
    /// `sources`, and stamps each input the loader and the compile read apart from the analysis. Those are the
    /// `composer.json` of each PHP# source's folder, or of the nearest folder above it that holds one, as the loader
    /// reads it, each absent one below it, Composer's `installed.json`, the baseline, and the `.sharp` folder of each
    /// folder that holds a package's PHP# file, absent unless the compile refuses it. `filesystems` tells which
    /// filesystems record every write, for the outputs' statuses. Gives none when the temp file can't be created or an
    /// input can't be stamped, and the compile then keeps no record.
    fn start(
        configuration: &Configuration,
        root: &Path,
        sources: &BTreeMap<String, Found>,
        folder: &Path,
        mut filesystems: Filesystems,
    ) -> Option<Self> {
        std::fs::create_dir_all(folder).ok()?;
        let marker_file = temp_file_in(folder).ok()?;
        let marker = Status::of(marker_file.path(), &marker_file.as_file().metadata().ok()?, &mut filesystems).1;
        let stamp = |path: &[u8]| unit::stamp(root, path).ok().map(|input| recorded(path, input.as_ref()));
        let mut inputs = Vec::new();
        let mut folders = HashSet::new();
        for name in sources.keys().filter(|name| name.ends_with(".sharp")) {
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
            files: sources.iter().map(|(name, found)| (name.clone(), (found.clone(), [0; 16]))).collect(),
            inputs,
            outputs: BTreeMap::new(),
            marker_file: Some(marker_file),
            warnings: logger::warnings(),
            lost: false,
            filesystems,
        })
    }

    /// The path of the record's own temp file, which the compile keeps in its folder until it is done.
    fn marker_path(&self) -> Option<PathBuf> {
        self.marker_file.as_ref().map(|file| file.path().to_path_buf())
    }

    /// Keeps the file at `path`, which the compile just wrote or kept with `bytes`, as an output with its status now.
    fn output(&mut self, path: &Path, bytes: &[u8]) {
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let name = source_name(&self.root, path);
                let status = Status::of(path, &metadata, &mut self.filesystems);
                self.files.remove(&name);
                self.outputs.insert(name, (status, unit::source_hash(bytes)));
            }
            Err(_) => self.lost = true,
        }
    }

    /// Keeps the file at `path`, which the compile read, as an input with its stamp now.
    fn input(&mut self, path: &Path) {
        let name = source_name(&self.root, path);
        match unit::stamp(&self.root, name.as_bytes()) {
            Ok(input) => self.inputs.push(recorded(name.as_bytes(), input.as_ref())),
            Err(_) => self.lost = true,
        }
    }

    /// Writes the record to `path` once the compile succeeded and printed `summary`: hashes each source file's bytes as
    /// `database` read them, or as the file holds them when the database didn't read it, and adds `inputs`, the stamps
    /// the compile took. A compile that warned keeps no record, so the next one prints the warning again, and neither
    /// does one whose output changed since it wrote or kept it, as a file someone swapped in or a compile that ran at
    /// once changes it.
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
    ) -> Result<(), Error> {
        let changed = self.outputs.iter().any(|(name, (status, _))| {
            let output = self.root.join(name);
            std::fs::metadata(&output)
                .map_or(true, |metadata| Status::of(&output, &metadata, &mut self.filesystems) != *status)
        });
        if self.lost || changed || logger::warnings() != self.warnings {
            return Ok(());
        }
        let mut files = BTreeMap::new();
        for (name, (found, _)) in std::mem::take(&mut self.files) {
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
            files.insert(name, (found, hash));
        }
        self.inputs.extend(inputs);
        self.inputs.retain(|(path, ..)| !files.contains_key(path));
        self.inputs.sort_unstable();
        self.inputs.dedup();
        self.files = files;
        self.summary = summary;
        let Some(file) = self.marker_file.take() else {
            return Ok(());
        };

        match rename_into(path, file, &serde_json::to_vec(&self)?) {
            // A compile that ran at once deleted the record's temp file with the other files it didn't write, so the
            // folder may hold its outputs, and this compile keeps no record.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            written => Ok(written.map_err(|error| DatabaseError::io(IOAction::WriteFile, path, error))?),
        }
    }
}

/// A record that is never written deletes its temp file, and then its folder when that holds nothing else, as the
/// `.sharp` folder of a project whose first compile refused every file doesn't, so no compile leaves an empty folder.
impl Drop for Record {
    fn drop(&mut self) {
        if let Some(file) = self.marker_file.take() {
            let folder = file.path().parent().map(Path::to_path_buf);
            drop(file);
            if let Some(folder) = folder {
                // Fails, as it should, while the folder holds a file.
                let _ = std::fs::remove_dir(folder);
            }
        }
    }
}

/// Whether `record` stands for a compile of the project at `root` with `configuration` now, whose source files are
/// `sources`: its header names this checker build, `root` and `configuration`, it holds the same source files at the
/// same real paths, each output is unchanged, each other input passes `unit::stamp`, and each source file is unchanged.
/// A source file is unchanged when its status is and both its times are older than the record's marker, or else when
/// its bytes hash as the compile's did. A status that changed only makes the file's bytes hashed, as git hashes a
/// racily clean file of its index, so a write in the same tick as the compile's read is caught. An output is unchanged
/// when its status is, on a filesystem whose change time records every write, or else when its bytes hash the same.
/// `filesystems` tells which filesystems record every write. A record that can't be checked is not current, and the
/// full compile reports what fails.
fn every_input_is_current(
    record: &Record,
    configuration: &Configuration,
    root: &Path,
    sources: &BTreeMap<String, Found>,
    filesystems: &mut Filesystems,
) -> bool {
    let found: Vec<(&String, &Found)> =
        sources.iter().filter(|(name, _)| !record.outputs.contains_key(name.as_str())).collect();
    let hashes_as =
        |path: &Path, hash: &[u8; 16]| std::fs::read(path).is_ok_and(|bytes| unit::source_hash(&bytes) == *hash);

    record.build == BUILD_ID.to_be_bytes()
        && record.root == root
        && record.configuration == configuration_hash(configuration)
        && found.len() == record.files.len()
        && found.iter().zip(&record.files).all(|((name, Found(_, real)), (recorded, (Found(_, recorded_real), _)))| {
            *name == recorded && real == recorded_real
        })
        && record.outputs.iter().all(|(name, (status, hash))| {
            let output = root.join(name);
            std::fs::metadata(&output)
                .is_ok_and(|metadata| status.sees_writes() && Status::of(&output, &metadata, filesystems) == *status)
                || hashes_as(&output, hash)
        })
        && record.inputs.iter().all(|(path, size, hash)| match unit::stamp(root, path.as_bytes()) {
            Ok(Some(input)) => (input.size, input.hash) == (*size, *hash),
            // On a disk that ignores case, the loader reads a file whose name differs only in case.
            Ok(None) => (*size, *hash) == (0, [0; 16]) && !root.join(path).exists(),
            Err(_) => false,
        })
        && found.iter().zip(record.files.values()).all(|((name, Found(status, _)), (Found(recorded, _), hash))| {
            (status == recorded && status.is_older_than(record.marker)) || hashes_as(&root.join(name), hash)
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

/// A source file as [`source_files`] finds it: its status, and the name of its real path when that differs from its
/// own, as a file reached through a link has.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Found(Status, Option<String>);

/// A file's size, modification time, status change time, inode and device, each time in nanoseconds since the epoch.
///
/// Where the filesystem's status change time records every write, as APFS, ext4 and most others do, a write changes it,
/// a time `touch` sets back included, so a file whose status is unchanged and whose times are older than a compile's
/// start holds the bytes the compile read. Where it doesn't, as on FAT and exFAT and on every system but macOS and
/// Linux, where it is not read, the status change time is `i64::MAX`, so the file's bytes are always hashed.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Status(u64, i64, i64, u64, u64);

impl Status {
    /// The status of the file at `path` from its `metadata`, its status change time read only when `filesystems` says
    /// its filesystem records every write in it.
    #[cfg(unix)]
    fn of(path: &Path, metadata: &std::fs::Metadata, filesystems: &mut Filesystems) -> Self {
        use std::os::unix::fs::MetadataExt;

        let nanoseconds = |seconds: i64, fraction: i64| seconds.saturating_mul(1_000_000_000).saturating_add(fraction);
        let records_writes = *filesystems.entry(metadata.dev()).or_insert_with(|| change_time_records_writes(path));

        Self(
            metadata.size(),
            nanoseconds(metadata.mtime(), metadata.mtime_nsec()),
            if records_writes { nanoseconds(metadata.ctime(), metadata.ctime_nsec()) } else { i64::MAX },
            metadata.ino(),
            metadata.dev(),
        )
    }

    #[cfg(not(unix))]
    fn of(_: &Path, metadata: &std::fs::Metadata, _: &mut Filesystems) -> Self {
        Self(metadata.len(), 0, i64::MAX, 0, 0)
    }

    /// Whether both the file's times are older than `marker`.
    fn is_older_than(self, marker: i64) -> bool {
        self.1 < marker && self.2 < marker
    }

    /// Whether the file's filesystem records every write in its status change time, so an unchanged status means
    /// unchanged bytes.
    fn sees_writes(self) -> bool {
        self.2 != i64::MAX
    }
}

/// Whether each filesystem, by its device, records every write in a file's status change time.
type Filesystems = HashMap<u64, bool>;

/// Whether the filesystem that holds `path` records every write in a file's status change time, as `statfs` names
/// it: every one but FAT and exFAT, which keep no status change time. One `statfs` can't name is taken not to.
#[cfg(target_os = "macos")]
fn change_time_records_writes(path: &Path) -> bool {
    filesystem_of(path).is_some_and(|filesystem| {
        let name: Vec<u8> =
            filesystem.f_fstypename.iter().take_while(|byte| **byte != 0).map(|byte| *byte as u8).collect();

        records_writes(&name)
    })
}

/// Whether the filesystem macOS names `name` records every write in a file's status change time.
#[cfg(target_os = "macos")]
fn records_writes(name: &[u8]) -> bool {
    !matches!(name, b"msdos" | b"exfat")
}

#[cfg(target_os = "linux")]
fn change_time_records_writes(path: &Path) -> bool {
    filesystem_of(path).is_some_and(|filesystem| records_writes(filesystem.f_type as u64))
}

/// Whether the filesystem of the Linux magic number `magic` records every write in a file's status change time: every
/// one but FAT's `MSDOS_SUPER_MAGIC` and exFAT's `EXFAT_SUPER_MAGIC`.
#[cfg(target_os = "linux")]
fn records_writes(magic: u64) -> bool {
    !matches!(magic, 0x4d44 | 0x2011_bab0)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn change_time_records_writes(_: &Path) -> bool {
    false
}

/// The filesystem that holds `path`, as `statfs` gives it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn filesystem_of(path: &Path) -> Option<libc::statfs> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` ends in a NUL byte, and `filesystem` has room for the `statfs` the call fills in.
    if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return None;
    }

    // SAFETY: the call succeeded, so it filled `filesystem` in.
    Some(unsafe { filesystem.assume_init() })
}

/// The hash of `configuration`, from its debug form, which holds every field, the ones serde skips included: the
/// deprecated `perform-heuristic-checks` and the formatter's settings.
fn configuration_hash(configuration: &Configuration) -> [u8; 16] {
    unit::source_hash(format!("{configuration:?}").as_bytes())
}

/// Each file the loader could read under the configured `paths`, `includes` and `patches` roots, by the name the
/// database gives it, as [`Found`] holds it: a file a root names, whatever its extension, and each file of a configured
/// extension under a root folder or under the folder a glob pattern starts with, following links as the loader does.
/// The excludes are ignored, which only adds files, and the `.sharp` folder at `root` is left out, as the loader leaves
/// it out. `filesystems` tells which filesystems record every write.
fn source_files(configuration: &Configuration, root: &Path, filesystems: &mut Filesystems) -> BTreeMap<String, Found> {
    let source = &configuration.source;
    let whole_workspace = [".".to_owned()];
    let paths = if source.paths.is_empty() { &whole_workspace[..] } else { &source.paths[..] };
    let is_glob = |text: &str| text.contains(['*', '?', '[', '{']);
    let mut walk = Walk { root, extensions: &source.extensions, filesystems, files: BTreeMap::new() };
    for configured in paths.iter().chain(&source.includes).chain(&source.patches) {
        let path = root.join(configured);
        if path.exists() {
            let path = path.canonicalize().unwrap_or(path);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => walk.found(&path, &path, &metadata),
                Ok(_) => walk.folder(&path, &mut vec![path.clone()], false),
                Err(_) => {}
            }
        } else if is_glob(configured) {
            let start: PathBuf =
                path.components().take_while(|part| !is_glob(&part.as_os_str().to_string_lossy())).collect();
            let start = start.canonicalize().unwrap_or(start);
            walk.folder(&start, &mut vec![start.clone()], true);
        }
    }

    walk.files
}

/// The walk of [`source_files`] under `root`, which keeps each file of one of `extensions` it finds in `files`.
struct Walk<'walk> {
    root: &'walk Path,
    extensions: &'walk [String],
    filesystems: &'walk mut Filesystems,
    files: BTreeMap<String, Found>,
}

impl Walk<'_> {
    /// Walks `folder`, whose real path is the last of `real`, as the real paths of the folders above it in the walk
    /// come before it. A folder whose real path `real` holds is a link back to one of them, which the walk skips as the
    /// loader does. Each file is kept by its name, or by the name of its real path when `by_real_path` is set, as the
    /// loader names a file a glob pattern matches.
    fn folder(&mut self, folder: &Path, real: &mut Vec<PathBuf>, by_real_path: bool) {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // A link whose target is gone is no file, as the loader skips it.
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let real_path = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => path.canonicalize().unwrap_or_else(|_| path.clone()),
                _ => real.last().map_or_else(|| path.clone(), |parent| parent.join(entry.file_name())),
            };
            if metadata.is_dir() {
                if path != self.root.join(unit::COMPILED_FOLDER) && !real.contains(&real_path) {
                    real.push(real_path);
                    self.folder(&path, real, by_real_path);
                    real.pop();
                }
            } else if path
                .extension()
                .is_some_and(|extension| self.extensions.iter().any(|known| extension == known.as_str()))
            {
                let named = if by_real_path { real_path.clone() } else { path };
                self.found(&named, &real_path, &metadata);
            }
        }
    }

    /// Keeps the file the walk reached at `path`, whose real path is `real`.
    fn found(&mut self, path: &Path, real: &Path, metadata: &std::fs::Metadata) {
        let name = source_name(self.root, path);
        let real = Some(source_name(self.root, real)).filter(|real| *real != name);
        self.files.insert(name, Found(Status::of(path, metadata, self.filesystems), real));
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
/// against, and when the package is a release other than mago's own, since mago reads the library it ships with, as
/// the `Dispatcher` it marks. A Composer dev version, as `dev-master` or `0.6.x-dev`, and a library outside `vendor/`
/// name no release, so they compile.
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
    // A tag names its release with a `v`, as the engine reads it.
    let release =
        version.strip_prefix('v').filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit())).unwrap_or(&version);
    if !release.is_empty() && !release.starts_with("dev-") && !release.ends_with("-dev") && release != VERSION {
        return Err(format!(
            "This project's PHP# standard library is {release}, and mago is {VERSION}. Move mago and the standard library \
             to the same release: require {STANDARD_LIBRARY_PACKAGE} {VERSION}, or heyjordanparker/mago-sharp {release}."
        ));
    }

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

/// The PHP file beside the PHP# source `declared`, spec section 27. PHP code that lists `*.php` files and names each
/// class by its path finds the PHP# class through it, and Composer's PSR-4 loads it before the source. It loads the
/// source through the engine, and on plain PHP it throws before any source can reach the output. Its last statement
/// states the type's declaration and never runs, so a tool that reads PHP source, as Composer's class map and
/// Symfony's attribute routing do, finds the type in it, and the engine declares the type once, from the source. PHP
/// refuses a keyword as a type's name even in a declaration that never runs, so the file of a type named after one, as
/// `Case`, states none and parses. Once the source is gone it declares nothing, as if it were not there, until the next
/// compile deletes it.
///
/// A project's formatter passes it as written: `mago fmt`, Pint's `laravel`, `per` and `psr12` presets, and
/// PHP-CS-Fixer's `@PER-CS2x0`. Pint's `laravel` preset wants each global name a namespace uses imported. PHPCS's
/// `PSR12` warns of the side effects a generated file has, so the file marks itself `phpcs:ignoreFile`. A type named
/// `Error` declares the name the file would import, which PHP refuses even in a declaration that never runs, so its
/// file names `\Error` in full. PHP warns of an import in no namespace, where every global name resolves as written, so
/// there the file imports none.
fn beside(declared: &DeclaredType) -> String {
    let DeclaredType { file, name, keyword } = declared;
    let stem = file.strip_suffix(".sharp").unwrap_or(file);
    let (imports, error) = match name.rsplit_once('.') {
        Some((namespace, _)) => {
            let namespace = namespace.replace('.', "\\");
            let functions = "use function function_exists;\nuse function is_file;\nuse function sprintf;\n\n";
            if stem.eq_ignore_ascii_case("Error") {
                (format!("namespace {namespace};\n\n{functions}"), "\\Error")
            } else {
                (format!("namespace {namespace};\n\nuse Error;\n\n{functions}"), "Error")
            }
        }
        None => (String::new(), "Error"),
    };
    let declaration = match keyword {
        Some(keyword) => format!(
            "\nif (false) {{\n    {keyword} {stem}\n    {{\n        // The engine declares {stem} from {file}, and this declaration never runs.\n    }}\n}}\n"
        ),
        None => String::new(),
    };

    format!(
        "{COMPANION_START}{file}, so PHP code that finds *.php files finds its type.\n// phpcs:ignoreFile\n\ndeclare(strict_types=1);\n\n{imports}if (is_file(sprintf('%s/{file}', __DIR__))) {{\n    function_exists('Sharp\\Internal\\requireNative')\n        || throw new {error}(\n            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',\n        );\n\n    require_once sprintf('%s/{file}', __DIR__);\n}}\n{declaration}"
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

/// The first line of the `.gitignore` [`keep_out_of_git`] writes, which marks the file as the compile's own.
const GITIGNORE_START: &str =
    "# mago compile writes this file and the files it lists. It rewrites it on every compile.\n";

/// Keeps `written`, the names of the PHP files the compile wrote in `folder`, out of git through the folder's
/// `.gitignore`, which lists itself and each of them. Each checkout of a repository has its own `.gitignore`, so git
/// ignores the files of this checkout alone, where the `info/exclude` every checkout shares hid them in all of them. A
/// file's name is a type's name and `.php`, which holds no character a pattern reads as a wildcard.
///
/// The compile's own file, as [`gitignore_written_by_compile`] tells it, is rewritten when the list changes, and is
/// deleted once the folder holds no such file. A `.gitignore` the compile didn't write, tracked or not, is left as it
/// is, and the compile warns of each file it doesn't list, as [`lists`] reads it, since git may show that file as
/// untracked. So it does of a `.gitignore` that is a link, which git doesn't read, and of one it can't read, which fails
/// no compile.
fn keep_out_of_git(root: &Path, folder: &Path, written: &BTreeSet<String>) -> Result<(), DatabaseError> {
    let path = folder.join(".gitignore");
    let relative = folder.strip_prefix(root).unwrap_or(folder);
    let shown = relative.join(".gitignore");
    let shown = shown.display();
    // The files git may show for `names`, and the lines that list them, each joined by commas.
    let named = |names: &[&String]| -> (String, String) {
        let files: Vec<String> = names.iter().map(|name| relative.join(name).display().to_string()).collect();
        let lines: Vec<String> = names.iter().map(|name| format!("/{name}")).collect();
        (files.join(", "), lines.join(", "))
    };
    let all: Vec<&String> = written.iter().collect();
    if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        if !all.is_empty() {
            let (files, lines) = named(&all);
            tracing::warn!(
                "{shown} is a link, which git doesn't read, so git may show {files} as untracked: make it a file that lists {lines}."
            );
        }
        return Ok(());
    }
    let before = match std::fs::read(&path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            if !all.is_empty() {
                let (files, _) = named(&all);
                tracing::warn!(
                    "{shown} can't be read: {}, so git may show {files} as untracked. Make it a file mago compile can read, and run vendor/bin/mago compile again.",
                    error.kind()
                );
            }
            return Ok(());
        }
    };
    match before {
        Some(theirs) if !gitignore_written_by_compile(&theirs) => {
            let unlisted: Vec<&String> = written.iter().filter(|name| !lists(&theirs, name)).collect();
            if !unlisted.is_empty() {
                let (files, lines) = named(&unlisted);
                tracing::warn!(
                    "{shown} doesn't list {lines}, so git may show {files} as untracked: add {lines} to it."
                );
            }
        }
        Some(_) if written.is_empty() => {
            std::fs::remove_file(&path)
                .or_else(ignore_missing)
                .map_err(|error| DatabaseError::io(IOAction::DeleteFile, &path, error))?;
        }
        None if written.is_empty() => {}
        before => {
            let mut after = format!("{GITIGNORE_START}/.gitignore\n");
            for name in written {
                after.push_str(&format!("/{name}\n"));
            }
            if before.as_deref() != Some(after.as_bytes()) {
                write_by_rename(&path, after.as_bytes())?;
            }
        }
    }

    Ok(())
}

/// Whether `contents` is a `.gitignore` [`keep_out_of_git`] wrote: [`GITIGNORE_START`], then lines that each name the
/// file itself or a PHP file, as `/` and its name. Any other line makes the file someone else's, so the compile never
/// rewrites or deletes a line someone added.
fn gitignore_written_by_compile(contents: &[u8]) -> bool {
    contents.strip_prefix(GITIGNORE_START.as_bytes()).is_some_and(|lines| {
        lines.split_inclusive(|byte| *byte == b'\n').all(|line| {
            line.strip_suffix(b"\n").and_then(|line| line.strip_prefix(b"/")).is_some_and(|name| {
                name == b".gitignore"
                    || name
                        .strip_suffix(b".php")
                        .is_some_and(|stem| !stem.is_empty() && stem.iter().all(is_part_of_identifier))
            })
        })
    })
}

/// Whether git reads the `.gitignore` `contents`, which someone else wrote, as ignoring the PHP file `name` of its
/// folder: a line names it as `/` and its name, and no `!` line after that names it again, which git reads as showing
/// it. A byte order mark before the first line, and the spaces and the carriage return that end a line, are no part of
/// the line.
fn lists(contents: &[u8], name: &str) -> bool {
    let contents = contents.strip_prefix(b"\xef\xbb\xbf").unwrap_or(contents);
    let mut listed = false;
    for line in contents.split(|byte| *byte == b'\n').map(<[u8]>::trim_ascii_end) {
        match line.strip_prefix(b"!") {
            Some(shown) => listed &= shown.strip_prefix(b"/").unwrap_or(shown) != name.as_bytes(),
            None => listed |= line.strip_prefix(b"/") == Some(name.as_bytes()),
        }
    }

    listed
}

/// Adds to `folders` each folder under the project's own source `paths` that holds a `.gitignore` the compile wrote, so
/// the compile comes back to a folder whose every source is gone even when none of them was compiled, as a refused one
/// isn't. `configuration` is the one the analysis walks the project's own files by. A path that holds a wildcard is
/// walked from its folder before the wildcard, and no path walks the workspace. The walk skips each folder that walk
/// skips, as [`ExcludedFolders`] tells it: the `excludes`, the `includes` and the `.sharp` folder. It skips `vendor/`
/// too, and a path inside a skipped folder, where the compile writes no PHP file, each `.git` folder, and every link,
/// which the compile never writes through. A folder it can't open is skipped, as the analysis's own walk skips it with
/// a warning.
fn gitignore_folders(
    root: &Path,
    configuration: &DatabaseConfiguration<'_>,
    folders: &mut BTreeSet<PathBuf>,
) -> Result<(), DatabaseError> {
    let excluded = ExcludedFolders::from_configuration(configuration)?;
    let vendor = root.join("vendor");
    let skipped = |folder: &Path| folder.starts_with(&vendor) || excluded.contains(folder);
    let mut pending: Vec<PathBuf> = if configuration.paths.is_empty() {
        vec![root.to_path_buf()]
    } else {
        configuration
            .paths
            .iter()
            .map(|path| {
                // The folder a configured path names, before any wildcard, with its `.` parts gone.
                let path = String::from_utf8_lossy(path);
                let literal = match path.find(['*', '?', '[', '{']) {
                    Some(wildcard) => &path[..path[..wildcard].rfind('/').unwrap_or(0)],
                    None => path.as_ref(),
                };
                root.join(literal).components().collect::<PathBuf>()
            })
            .filter(|folder| !skipped(folder))
            .collect()
    };
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if entry.file_name() != ".git" && !skipped(&path) {
                    pending.push(path);
                }
            } else if file_type.is_file()
                && entry.file_name() == ".gitignore"
                && std::fs::read(&path).is_ok_and(|contents| gitignore_written_by_compile(&contents))
            {
                folders.insert(folder.clone());
            }
        }
    }

    Ok(())
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
    use mago_database::ReadDatabase;
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

    /// A project with `app/Order.sharp`, and `app/Paced.sharp`, which calls `Acme\Rate` in `vendor/`, and its
    /// configuration.
    fn project() -> (tempfile::TempDir, PathBuf, Configuration) {
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

        (directory, root, configuration)
    }

    /// A `vendor/` file a PHP# file calls is no input of its compiled file, which `composer.lock` stands for, so the
    /// record of the hosts' compile's source files is what sees an edit of it.
    #[test]
    fn the_hosts_compile_runs_again_after_an_edit_of_a_vendor_file() {
        let (_directory, root, configuration) = project();
        let folder = tempfile::tempdir().unwrap();
        assert!(compile_into(&configuration, &root, folder.path()).unwrap(), "a fresh folder is written");
        // A compile that runs writes its record anew, though every compiled file it writes is the same.
        let recorded = || std::fs::metadata(folder.path().join(INPUTS)).unwrap().modified().unwrap();
        let written = recorded();
        assert!(!compile_into(&configuration, &root, folder.path()).unwrap(), "nothing changed");
        assert_eq!(recorded(), written, "the compile had nothing to do");

        write(&root, "vendor/acme/rate/src/Rate.php", &rate("string"));
        let paced = std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap();

        assert!(compile_into(&configuration, &root, folder.path()).unwrap(), "the vendor edit compiles again");
        assert_ne!(std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap(), paced);
    }

    /// The hosts' compile refuses a standard library of another release as `mago compile` does, with the same message,
    /// where it once compiled against no library.
    #[test]
    fn the_hosts_compile_refuses_a_standard_library_of_another_release() {
        let (_directory, root, configuration) = project();
        let library = "vendor/heyjordanparker/php-sharp-composer";
        write(&root, &format!("{library}/composer.json"), "{\"name\": \"heyjordanparker/php-sharp-composer\"}\n");
        write(
            &root,
            &format!("{library}/library/Sharp/Math.sharp"),
            "namespace Sharp;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
        );
        write(
            &root,
            "vendor/composer/installed.json",
            "{\"packages\": [{\"name\": \"heyjordanparker/php-sharp-composer\", \"version\": \"v0.5.9\"}]}\n",
        );
        let folder = tempfile::tempdir().unwrap();

        let Err(error) = compile_into(&configuration, &root, folder.path()) else {
            panic!("the hosts' compile accepted library 0.5.9");
        };
        assert_eq!(
            error.to_string(),
            format!(
                "Orchestrator error: This project's PHP# standard library is 0.5.9, and mago is {VERSION}. Move mago and \
                 the standard library to the same release: require heyjordanparker/php-sharp-composer {VERSION}, or \
                 heyjordanparker/mago-sharp 0.5.9."
            )
        );
        assert!(!folder.path().join("app/Order.sharpc").exists());
    }

    /// Writes `bytes` to the file at `path` and sets its modification time back to `modified`, as `touch -d` does.
    fn rewrite(path: &Path, bytes: &[u8], modified: std::time::SystemTime) {
        std::fs::write(path, bytes).unwrap();
        std::fs::File::options().write(true).open(path).unwrap().set_modified(modified).unwrap();
    }

    /// On a filesystem whose change time records no write, as FAT's and exFAT's, an unchanged status says nothing of a
    /// file's bytes, so the check hashes each source file and each output: a rewrite of either that keeps its size and
    /// sets its modification time back makes the record stale.
    #[cfg(unix)]
    #[test]
    fn the_check_hashes_every_file_on_a_filesystem_whose_change_time_records_no_write() {
        use std::os::unix::fs::MetadataExt;

        let (_directory, root, configuration) = project();
        let folder = tempfile::tempdir().unwrap();
        let folder = folder.path();
        let output = folder.join("app/Order.sharpc");
        write(folder, "app/Order.sharpc", "compiled");
        let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for path in [root.join("app/Order.sharp"), output.clone()] {
            rewrite(&path, &std::fs::read(&path).unwrap(), an_hour_ago);
        }
        let mut forced = Filesystems::new();
        for path in [&root, folder] {
            forced.insert(std::fs::metadata(path).unwrap().dev(), false);
        }
        let sources = source_files(&configuration, &root, &mut forced);
        let mut record = Record::start(&configuration, &root, &sources, folder, forced.clone()).unwrap();
        record.output(&output, b"compiled");
        record.write(&folder.join(INPUTS), String::new(), &ReadDatabase::empty(), []).unwrap();
        let mut current = || {
            let sources = source_files(&configuration, &root, &mut forced);
            every_compiled_file_is_current(&configuration, &root, folder, &sources, &mut forced)
        };
        assert!(current(), "each file hashes as it did");

        for path in [root.join("app/Order.sharp"), output] {
            let bytes = std::fs::read(&path).unwrap();
            let mut changed = bytes.clone();
            *changed.last_mut().unwrap() ^= 1;

            rewrite(&path, &changed, an_hour_ago);
            assert!(!current(), "{} was rewritten", path.display());

            rewrite(&path, &bytes, an_hour_ago);
            assert!(current(), "{} holds its bytes again", path.display());
        }
    }

    /// `statfs` names FAT and exFAT as the filesystems whose change time records no write.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn statfs_tells_the_filesystems_whose_change_time_records_no_write() {
        #[cfg(target_os = "macos")]
        let (fat, exfat, others) =
            (b"msdos".as_slice(), b"exfat".as_slice(), [b"apfs".as_slice(), b"hfs".as_slice(), b"nfs".as_slice()]);
        #[cfg(target_os = "linux")]
        let (fat, exfat, others) = (0x4d44, 0x2011_bab0, [0xef53, 0x9123_683e, 0x0102_1994]);

        assert!(!records_writes(fat));
        assert!(!records_writes(exfat));
        assert!(others.into_iter().all(records_writes));
        let directory = tempfile::tempdir().unwrap();
        assert!(change_time_records_writes(directory.path()), "the temp folder's filesystem records every write");
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
