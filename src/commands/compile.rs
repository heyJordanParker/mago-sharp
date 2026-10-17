//! `mago compile`: checks every PHP# file, the ones in `vendor/` too, and writes the `.sharpc` file the engine runs
//! for each accepted one.
//!
//! Every compiled file lives in one `.sharp` folder at the workspace root, at the real path of its source relative to
//! the root, with `.sharp` replaced by `.sharpc`. The folder mirrors the accepted sources exactly: a refused, removed
//! or renamed source loses its compiled file.
//!
//! Beside each of the project's own PHP# sources that declares a type, outside `vendor/` and `autoload-dev` folders, it
//! also writes a PHP file of the type's name that loads the source, so PHP code that finds classes by listing `*.php`
//! files finds PHP# classes too, spec section 27. A removed source loses its PHP file, and git's `info/exclude` keeps
//! the files no `.gitignore` covers out of git.

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

use mago_allocator::LocalArena;
use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_database::file::FolderNamespace;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_orchestrator::service::incremental_analysis::compile::DispatcherModule;
use mago_orchestrator::service::incremental_analysis::compile::Listener;
use mago_prelude::Prelude;
use mago_reporting::IssueCollection;
use mago_reporting::color::ColorChoice as ReportingColorChoice;
use mago_reporting::reporter::Reporter;
use mago_reporting::reporter::ReporterConfig;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_sharp_lean::Lean;
use mago_sharp_lean::PACKAGE_FOLDER;
use mago_sharp_lean::rules_files;
use mago_syntax::cst::Statement;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;

use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::args::reporting::default_reporting_format;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
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

/// Command that compiles every PHP# file into the `.sharp` folder.
#[derive(Parser, Debug, Default)]
#[command(name = "compile")]
pub struct CompileCommand {}

impl CompileCommand {
    /// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every
    /// other file in the `.sharp` folder.
    ///
    /// Fails when a file is refused, when a source is a link to a file outside the workspace, when a package in
    /// `vendor/` holds a `.sharp` folder of its own, or when a PHP file someone wrote sits where a source's PHP file
    /// goes. Every accepted file is written either way.
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
        let mut orchestrator = create_orchestrator(&configuration, color_choice, false, true, false);
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

        let workspace = configuration.source.workspace.as_path();
        let root =
            workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.to_path_buf(), error))?;
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
            let Some((type_file, class)) = declared_type(&file) else {
                continue;
            };
            let php = format!("{}.php", type_file.strip_suffix(".sharp").unwrap_or(&type_file));
            let listing = match listings.entry(folder.clone()) {
                Entry::Occupied(listing) => listing.into_mut(),
                Entry::Vacant(listing) => listing.insert(file_names(&folder).map_err(DatabaseError::from)?),
            };
            // PHP names a class whatever its case, and a disk that ignores case opens `order.php` for `Order.php`.
            let named: Vec<&String> = listing.iter().filter(|name| name.eq_ignore_ascii_case(&php)).collect();
            let mut someone_wrote = None;
            for name in &named {
                if !written_by_compile(&folder.join(name)).map_err(DatabaseError::from)? {
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
                std::fs::remove_file(folder.join(name)).or_else(ignore_missing).map_err(DatabaseError::from)?;
            }
            let path = folder.join(&php);
            let contents = beside(&type_file);
            if !listing.contains(&php) || std::fs::read(&path).map_err(DatabaseError::from)? != contents.as_bytes() {
                write_by_rename(&path, contents.as_bytes()).map_err(DatabaseError::from)?;
            }
            written_beside.insert(path);
        }
        // A removed source's PHP file is in a folder that holds a source, or held one at the last compile.
        let compiled_folder = root.join(unit::COMPILED_FOLDER);
        source_folders(&root, &compiled_folder, &compiled_folder, &mut folders).map_err(DatabaseError::from)?;
        for folder in folders {
            for name in file_names(&folder).map_err(DatabaseError::from)? {
                let path = folder.join(&name);
                if name.ends_with(".php")
                    && !written_beside.contains(&path)
                    && written_by_compile(&path).map_err(DatabaseError::from)?
                {
                    std::fs::remove_file(&path).or_else(ignore_missing).map_err(DatabaseError::from)?;
                }
            }
        }
        git::exclude(&root, "mago compile", &written_beside.into_iter().collect::<Vec<_>>())?;

        let mut server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator));
        server.analyze()?;
        let lean = Lean::new(&root);
        let compilations = server.compile(
            |path| unit::stamp(&root, path),
            |name| rules_files(&root, name),
            |translations, namespaces| lean.check(translations, namespaces),
        )?;
        let database = server.database();

        let mut written = BTreeSet::new();
        let mut accepted = HashSet::new();
        let mut refused = IssueCollection::default();
        let mut refused_files = 0;
        let mut packaged_refusals = Vec::new();
        for (file_id, compilation) in compilations {
            let file = database.get_ref(&file_id)?;
            let name = String::from_utf8_lossy(&file.name);
            let source = file.path.clone().unwrap_or_else(|| root.join(name.as_ref()));
            let Some(compiled) = unit::compiled_path(&root, &source).map_err(DatabaseError::from)? else {
                tracing::error!(
                    "{name} is a link to {}, outside the project, so it was not compiled. Install the package as a copy instead of a link.",
                    source.canonicalize().map_err(DatabaseError::from)?.display()
                );
                failed = true;
                continue;
            };

            match compilation {
                Compilation::Accepted(bytes) => {
                    write_by_rename(&compiled, &bytes).map_err(DatabaseError::from)?;
                    written.insert(compiled);
                    accepted.insert(file_id);
                }
                Compilation::Refused(errors) => {
                    refused.extend(errors);
                    refused_files += 1;
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
            write_by_rename(&table, &listener_table(&inputs, &accepted, server.dispatcher_module().as_ref()))
                .map_err(DatabaseError::from)?;
            written.insert(table);
        }
        if compiled_folder.is_dir() {
            delete_all_but(&compiled_folder, &written, &compiled_folder.join(PACKAGE_FOLDER))
                .map_err(DatabaseError::from)?;
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

        let summary = match (accepted.len(), refused_files) {
            (0, 0) => "No PHP# files found to compile.".to_string(),
            (compiled, 0) => format!("Compiled {} into .sharp/.", files(compiled)),
            (compiled, refused) => format!(
                "Compiled {} into .sharp/. {} refused: fix {} errors and run vendor/bin/mago compile again.",
                files(compiled),
                if refused == 1 { "1 PHP# file was".to_string() } else { format!("{refused} PHP# files were") },
                if refused == 1 { "its" } else { "their" },
            ),
        };
        if failed || refused_files > 0 {
            tracing::error!("{summary}");

            return Ok(ExitCode::FAILURE.into());
        }
        tracing::info!("{summary}");

        Ok(ExitCode::SUCCESS.into())
    }
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
        inputs.extend(unit::stamp(root, &path).map_err(DatabaseError::from)?);
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

/// The start of each PHP file `mago compile` writes beside a PHP# source, which tells it from a file someone wrote.
const BESIDE: &str = "<?php\n\n// mago compile writes this file from ";

/// The PHP file beside the PHP# source `type_file`, spec section 27, in `mago fmt`'s layout. PHP code that lists
/// `*.php` files and names each class by its path finds the PHP# class through it, and Composer's PSR-4 loads it before
/// the source. It loads the source through the engine, and on plain PHP it throws before any source can reach the
/// output. Once the source is gone it declares nothing, as if it were not there, until the next compile deletes it.
fn beside(type_file: &str) -> String {
    format!(
        "{BESIDE}{type_file}, so PHP code that lists *.php files finds its class.\n\ndeclare(strict_types=1);\n\nif (!is_file(__DIR__ . '/{type_file}')) {{\n    return;\n}}\n\nfunction_exists('Sharp\\Internal\\requireNative')\n    || throw new Error(\n        'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',\n    );\n\nrequire_once __DIR__ . '/{type_file}';\n"
    )
}

/// Whether the file at `path` is one `mago compile` wrote beside a source, which starts with [`BESIDE`].
fn written_by_compile(path: &Path) -> std::io::Result<bool> {
    let mut start = Vec::with_capacity(BESIDE.len());
    match std::fs::File::open(path) {
        Ok(file) => file.take(BESIDE.len() as u64).read_to_end(&mut start)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };

    Ok(start == BESIDE.as_bytes())
}

/// The name of the PHP# source `file`, as `Order.sharp`, and the type it declares, as `App.Order`, when it declares a
/// type named like it, spec section 27: a class, interface, enum, struct or event. A module's file and an `extern`
/// declaration file declare none.
fn declared_type(file: &File) -> Option<(String, String)> {
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
    let declares = program.statements.iter().chain(namespaced.into_iter().flatten()).any(|statement| match statement {
        Statement::Class(class) => class.name.value == stem.as_bytes(),
        Statement::Struct(r#struct) => r#struct.name.value == stem.as_bytes(),
        Statement::Enum(r#enum) => r#enum.name.value == stem.as_bytes(),
        Statement::Interface(interface) => interface.name.value == stem.as_bytes(),
        Statement::Event(event) => event.name.value == stem.as_bytes(),
        _ => false,
    });
    let class = match namespace.and_then(|namespace| namespace.name.as_ref()) {
        Some(namespace) => format!("{}.{stem}", String::from_utf8_lossy(namespace.value()).replace('\\', ".")),
        None => stem.to_string(),
    };

    declares.then(|| (type_file.to_string(), class))
}

/// The names of the files in `folder`, exactly as the folder holds them, or none when it is gone.
fn file_names(folder: &Path) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }

    Ok(names)
}

/// Adds to `folders` the folder of the source of each compiled file under `folder` of the compiled folder `compiled`,
/// outside `vendor/` and the folder the Lean step owns.
fn source_folders(root: &Path, compiled: &Path, folder: &Path, folders: &mut BTreeSet<PathBuf>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(compiled).unwrap_or(&path);
        if entry.file_type()?.is_dir() {
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
/// then every folder left empty, `folder` included.
fn delete_all_but(folder: &Path, kept: &BTreeSet<PathBuf>, owned: &Path) -> std::io::Result<bool> {
    let mut empty = true;
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if path == owned {
            empty = false;
        } else if entry.file_type()?.is_dir() {
            empty &= delete_all_but(&path, kept, owned)?;
        } else if kept.contains(&path) {
            empty = false;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    if empty {
        std::fs::remove_dir(folder)?;
    }

    Ok(empty)
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
    let mut folders = BTreeSet::new();
    for file in database.files().filter(|file| file.name.starts_with(VENDOR) && Dialect::of(file).is_sharp()) {
        let name = String::from_utf8_lossy(&file.name);
        let mut folder = name.as_ref();
        while let Some((parent, _)) = folder.rsplit_once('/') {
            let compiled = format!("{parent}/{}", unit::COMPILED_FOLDER);
            if root.join(&compiled).is_dir() {
                folders.insert(compiled);
            }
            folder = parent;
        }
    }

    folders
}
