//! `mago compile`: checks every PHP# file, the ones in `vendor/` too, and writes the `.sharpc` file the engine runs
//! for each accepted one.
//!
//! Every compiled file lives in one `.sharp` folder at the workspace root, at the real path of its source relative to
//! the root, with `.sharp` replaced by `.sharpc`. The folder mirrors the accepted sources exactly: a refused, removed
//! or renamed source loses its compiled file.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;

use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_prelude::Prelude;
use mago_reporting::IssueCollection;
use mago_reporting::color::ColorChoice as ReportingColorChoice;
use mago_reporting::reporter::Reporter;
use mago_reporting::reporter::ReporterConfig;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_sharp_lean::Lean;
use mago_sharp_lean::PACKAGE_FOLDER;
use mago_syntax::dialect::Dialect;

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

/// The folder Composer installs packages into.
const VENDOR: &[u8] = b"vendor/";

/// Command that compiles every PHP# file into the `.sharp` folder.
#[derive(Parser, Debug, Default)]
#[command(name = "compile")]
pub struct CompileCommand {}

impl CompileCommand {
    /// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every
    /// other file in the `.sharp` folder.
    ///
    /// Fails when a file is refused, when a source is a link to a file outside the workspace, or when a package in
    /// `vendor/` holds a `.sharp` folder of its own. Every accepted file is written either way.
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
                host
            })
            .collect();
        for file in packaged {
            database.add(file);
        }

        let mut server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator));
        server.analyze()?;
        let lean = Lean::new(&root);
        let compilations = server.compile(|path| unit::stamp(&root, path), |translations| lean.prove(translations))?;
        let database = server.database();

        let mut written = BTreeSet::new();
        let mut refused = IssueCollection::default();
        let mut refused_files = 0;
        let mut packaged_refusals = Vec::new();
        let mut failed = false;
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

        let compiled_folder = root.join(unit::COMPILED_FOLDER);
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

        match (written.len(), refused_files) {
            (0, 0) => tracing::info!("No PHP# files found to compile."),
            (compiled, 0) => tracing::info!("Compiled {} into .sharp/.", files(compiled)),
            (compiled, refused) => tracing::error!(
                "Compiled {} into .sharp/. {} refused: fix {} errors and run vendor/bin/mago compile again.",
                files(compiled),
                if refused == 1 { "1 PHP# file was".to_string() } else { format!("{refused} PHP# files were") },
                if refused == 1 { "its" } else { "their" },
            ),
        }

        Ok(if failed || refused_files > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }.into())
    }
}

/// `count` PHP# files, in words.
fn files(count: usize) -> String {
    if count == 1 { "1 PHP# file".to_string() } else { format!("{count} PHP# files") }
}

/// Writes `bytes` to a new file beside `path` and renames it over `path`, so a reader sees the old file or the new
/// one, never part of one.
fn write_by_rename(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path.parent().unwrap_or(path);
    std::fs::create_dir_all(folder)?;
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    file.write_all(bytes)?;
    let (file, written) = file.keep().map_err(|error| error.error)?;
    drop(file);

    // `NamedTempFile::persist` renames with `MoveFileExW` alone, which fails on Windows while a reader holds the old
    // file open. `std::fs::rename` then falls back to a POSIX rename, which replaces it when every reader opened it with
    // `FILE_SHARE_DELETE`, as Rust's `File::open` does, and leaves each reader the old file.
    std::fs::rename(&written, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&written);
    })
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
