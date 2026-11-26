//! `mago compile`: checks every PHP# file, the ones in `vendor/` too, and writes the `.sharpc` file the engine runs
//! for each accepted one.
//!
//! Every compiled file lives in one `.sharp` folder at the workspace root, at the real path of its source relative to
//! the root, with `.sharp` replaced by `.sharpc`. The folder mirrors the accepted sources exactly: a refused, removed
//! or renamed source loses its compiled file. It writes nothing outside that folder.
//!
//! It writes `preload.php` in the `.sharp` folder too, the script `opcache.preload` names to preload every type it
//! accepted outside `autoload-dev` folders, and `autoload.php`, which requires the project's Composer autoloader for a
//! program and for the preload script, spec section 27.
//!
//! A compile that warns of nothing beyond what it refused keeps a record of everything it read and wrote, and of what it
//! printed, in the `.sharp` folder, and the next compile checks it before any analysis: when every input and output is
//! as it was, it prints the same outcome, a failure's issues and errors included, exits as it did, and does nothing
//! else. A compile whose law checker refused a file keeps none, since what the checker does depends on no input. A
//! project with an extension host enabled compiles every time, since a host names none of what it reads.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use clap::ColorChoice;
use clap::Parser;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use serde::Deserialize;
use serde::Serialize;

use mago_allocator::LocalArena;
use mago_analyzer::error::AnalysisError;
use mago_analyzer::external::ExternalAnalyzerError;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::external::ReturnTypeRequests;
use mago_analyzer::plugin::PluginError;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::scanner::scan_program;
use mago_codex::scanner::var_tag_class_likes;
use mago_codex::symbol::SymbolKind;
use mago_composer::autoload::Autoload;
use mago_composer::autoload::AutoloadError;
use mago_composer::autoload::MAPS;
use mago_composer::autoload::vendor_folder;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::file::FolderNamespace;
use mago_database::loader::Listing;
use mago_database::loader::Manifests;
use mago_database::loader::nearest_manifest;
use mago_database::loader::read_listed;
use mago_database::membership::WorkspaceMatcher;
use mago_extension::WorkerError;
use mago_names::resolver::ChangingMembers;
use mago_names::resolver::ExternNamespaces;
use mago_names::resolver::NameResolver;
use mago_names::resolver::NamespaceClasses;
use mago_orchestrator::Autoloader;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::OrchestratorError;
use mago_orchestrator::service::incremental_analysis::Scans;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_orchestrator::service::incremental_analysis::compile::Compiled;
use mago_orchestrator::service::incremental_analysis::compile::CompiledFiles;
use mago_orchestrator::service::incremental_analysis::compile::ExternAttributes;
use mago_orchestrator::service::incremental_analysis::compile::ExternEffects;
use mago_orchestrator::service::incremental_analysis::compile::Family;
use mago_orchestrator::service::incremental_analysis::compile::Folder;
use mago_orchestrator::service::incremental_analysis::compile::Listener;
use mago_orchestrator::service::incremental_analysis::compile::RecordedModule;
use mago_orchestrator::service::phase::Phase;
use mago_prelude::Prelude;
use mago_reporting::CompiledIgnoreSet;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_reporting::ReportingFormat;
use mago_reporting::ReportingTarget;
use mago_reporting::baseline::Baseline;
use mago_reporting::color::ColorChoice as ReportingColorChoice;
use mago_reporting::reporter::Reporter;
use mago_reporting::reporter::ReporterConfig;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_sharp_lean::Lean;
use mago_sharp_lean::PACKAGE_FOLDER;
use mago_sharp_lean::PROOFS;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file_with_settings;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::baseline::unserialize_baseline;
use crate::commands::analyze::analyze_the_whole_workspace_without_paths;
use crate::commands::analyze::decode_prelude;
use crate::commands::analyze::server_settings;
use crate::commands::args::reporting::default_reporting_format;
use crate::commands::outcome::CommandOutcome;
use crate::config::Configuration;
use crate::config::extension::ExtensionHostConfiguration;
use crate::consts::BUILD_ID;
use crate::consts::PRELUDE_BYTES;
use crate::consts::VERSION;
use crate::error::Error;
use crate::extensions::start_external_analyzer;
use crate::utils::create_orchestrator;
use crate::utils::logger;
use crate::utils::rename_into;
use crate::utils::temp_file_in;
use crate::utils::write_by_rename;

/// The folder Composer installs packages into.
const VENDOR: &[u8] = b"vendor/";

/// The listener table in the `.sharp` folder, which the engine reads to run each PHP# listener, spec section 15. Every
/// compile of a project with a PHP# file writes it, listeners or none, so the engine can tell when it is out of date.
const LISTENERS: &str = "listeners.php";

/// The preload script in the `.sharp` folder, spec section 27, which `opcache.preload` names to preload every type
/// the compile accepted outside `autoload-dev` folders.
const PRELOAD: &str = "preload.php";

/// The script in the `.sharp` folder that requires the project's Composer autoloader, spec section 27, which a program
/// loads before it builds its class, and the preload script before the types it preloads.
const AUTOLOAD: &str = "autoload.php";

/// The record of everything the last compile read and wrote, in the `.sharp` folder and in the hosts' folder, which the
/// next compile checks before any analysis: when every input and output is as it was, the compile has nothing to do. A
/// compile that warned, was interrupted or had a file refused by the law checker leaves none, and so does one whose
/// output changed while it ran.
const INPUTS: &str = "inputs.json";

/// The record of what the last compile kept of each PHP# file it accepted, in the `.sharp` folder and in the hosts'
/// folder, as `CompiledFiles::encode` writes it, so the next compile keeps the compiled file of each file whose key
/// holds instead of lowering it again.
const COMPILED_FILES: &str = "compiled.bin";

/// The file in the `.sharp` folder whose lock the engine holds while it runs `sharp.compile_command`, so a project
/// compiles once at a time on demand, and in the hosts' folder whose lock [`compile_for_hosts`] takes the same way. The
/// compile keeps it, since a lock on a file it deleted would lock nothing.
const COMPILE_LOCK: &str = "compile.lock";

/// The file in the `.sharp` folder whose exclusive lock each run that writes the folder holds, from before it reads the
/// folder's record to after it deletes what it did not write, so two runs of one tree take turns on it. It is not
/// [`COMPILE_LOCK`], which the engine holds while the compile it runs takes this one. The compile keeps it.
const WRITE_LOCK: &str = "write.lock";

/// What this process holds of each hosts' folder it compiled into.
static HOSTS_FOLDERS: Mutex<BTreeMap<PathBuf, HeldFolder>> = Mutex::new(BTreeMap::new());

/// A hosts' folder as this process last held it.
struct HeldFolder {
    /// The lock of the folder's [`COMPILE_LOCK`], shared while the process's hosts may read the folder, and none
    /// between a watch's analyses. A process holds one descriptor per folder, since two descriptors of one process
    /// lock each other out.
    lock: Option<std::fs::File>,
    /// The hash of the folder's [`INPUTS`] when the process last held the folder, or none when the folder kept no
    /// record.
    record: Option<[u8; 16]>,
}

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
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
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
/// the folder through [`HOST_ROOT`] and [`HOST_COMPILED`], which every command starts its hosts from, and what changed
/// in the folder's tree since this process last held it, whichever run changed it, so a watch starts again the workers
/// that loaded a class that changed. The folder is the
/// project's in mago's cache, outside the project, and only the hosts' engine reads it, so the project's `.sharp`
/// folder holds only what `mago compile` accepted.
///
/// The compile runs without the hosts and lowers every file that lowers, whatever the analysis says of it: a host sees
/// the project as it is, as a Roslyn analyzer gets the compilation with its errors and `tsc` emits despite type errors.
/// A file that does not lower is in the folder's [`HOST_REPORT`], so a worker that loads it gets the checker's errors.
/// It runs only when a file is missing from the folder or out of date, and silences its own thread's log: the run with
/// the hosts that follows reports the project's errors.
///
/// Runs that start together take turns on the folder's [`COMPILE_LOCK`], as requests take turns on the engine's: a run
/// checks the folder under the shared lock, and one that finds it stale compiles under the exclusive lock, which it
/// takes once no other run's hosts read the folder. A run that waited for another's compile checks again and reuses
/// its tree. The process then holds the shared lock while its hosts read the folder, so no other run rewrites it under
/// them, until it exits or [`release_hosts_folders`] lets it go. The kernel frees the lock of a run that dies.
///
/// # Errors
///
/// Returns the error that stops the compile.
pub(crate) fn compile_for_hosts(
    configuration: &Configuration,
) -> Result<(BTreeMap<String, ExtensionHostConfiguration>, HostsChange), Error> {
    let mut hosts = HostsCompile::new(configuration)?;
    let compiled = if hosts.enabled {
        let phase = Phase::start("hosts");
        let compiled = silenced(|| hosts.compile_whole(false))?;
        phase.end();
        compiled
    } else {
        HostsChange::Unchanged
    };

    Ok((hosts.extension_hosts, compiled))
}

/// Runs `compile` with its own thread's log silenced, as the hosts' compile runs: the run with the hosts that follows
/// reports the project's errors.
fn silenced<T>(compile: impl FnOnce() -> T) -> T {
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), compile)
}

/// Lets go of the lock of each hosts' folder this process holds, so other runs compile into it while its hosts read
/// nothing, as a watch's hosts read nothing between its analyses. The next [`compile_for_hosts`] takes it again.
pub(crate) fn release_hosts_folders() {
    for held in HOSTS_FOLDERS.lock().unwrap_or_else(PoisonError::into_inner).values_mut() {
        held.lock = None;
    }
}

/// The source files whose compiled file in a hosts' folder a compile changed, and the classes they now declare.
pub(crate) struct Recompiled {
    /// Each file whose compiled file the compile wrote with other bytes, or refused while one was there.
    pub(crate) files: HashSet<FileId>,
    /// The classes those files declare after the compile, each by its lowercase name.
    pub(crate) classes: HashSet<Word>,
}

/// What changed in a hosts' folder since this process last held it.
pub(crate) enum HostsChange {
    /// No file in the folder changed.
    Unchanged,
    /// The compile of this process changed a file in the folder.
    Recompiled(Recompiled),
    /// Another run rewrote the folder, or it keeps no record, so any compiled file may have changed.
    Unknown,
}

/// The compile of a project for its extension hosts, as [`compile_for_hosts`] runs it, which a watch keeps between its
/// analyses. After its first compile it keeps that compile's analysis and record, so each [`Self::recompile`] analyzes
/// and lowers again only what the files that changed reach, stamps only their inputs, and writes only the compiled files
/// whose bytes changed.
pub(crate) struct HostsCompile {
    /// The project's configuration with no extension host, which the compile runs under.
    configuration: Configuration,
    /// The project's hosts, each enabled one given the folder through [`HOST_ROOT`] and [`HOST_COMPILED`].
    extension_hosts: BTreeMap<String, ExtensionHostConfiguration>,
    /// Whether a host is enabled, without which there is nothing to compile.
    enabled: bool,
    /// The project's canonical root.
    root: PathBuf,
    /// The hosts' folder in mago's cache.
    folder: PathBuf,
    /// What the last compile this process ran into the folder left, or none before the first.
    last: Option<LastCompile>,
}

/// What a compile for the hosts left in the process that ran it, which the next one starts from while no other run
/// compiled into the folder since.
struct LastCompile {
    /// The analysis it lowered from, without the hosts.
    server: Server,
    /// The autoloader of Composer's maps, which serves files the record stamps apart from the sources.
    autoloader: Option<std::sync::Arc<Autoloader>>,
    /// Which paths the project's database holds a file at.
    workspace: WorkspaceMatcher,
    /// The source files the record keeps, as [`source_files`] finds them.
    sources: BTreeMap<String, Found>,
    /// Whether each filesystem records every write.
    filesystems: Filesystems,
    /// The standard library the compiled files name.
    library: unit::Library,
    /// What it kept of each PHP# file it accepted.
    compiled_files: CompiledFiles,
    /// The status and hash of each file it wrote or kept, by name, as the record keeps them.
    outputs: Outputs,
    /// The stamp of each source file and source listing it stamped, which holds until the file or a file of the folder
    /// changes.
    stamps: HashMap<Vec<u8>, unit::Input>,
}

impl HostsCompile {
    /// The compile for the hosts of `configuration`'s project, which has compiled nothing yet.
    ///
    /// # Errors
    ///
    /// Returns the error of finding the project's root or mago's cache.
    pub(crate) fn new(configuration: &Configuration) -> Result<Self, Error> {
        let mut extension_hosts = configuration.extension_hosts.clone();
        let workspace = configuration.source.workspace.clone();
        if !extension_hosts.values().any(|host| host.enabled) {
            return Ok(Self {
                configuration: configuration.clone(),
                extension_hosts,
                enabled: false,
                root: workspace,
                folder: PathBuf::new(),
                last: None,
            });
        }

        let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.clone(), error))?;
        let project: String =
            unit::source_hash(root.as_os_str().as_encoded_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
        let folder = mago_sharp_lean::cache_root(&configuration.environment)
            .map_err(|error| OrchestratorError::General(error.to_string()))?
            .join("hosts")
            .join(project);
        for host in extension_hosts.values_mut().filter(|host| host.enabled) {
            host.environment.insert(HOST_ROOT.to_owned(), root.to_string_lossy().into_owned());
            host.environment.insert(HOST_COMPILED.to_owned(), folder.to_string_lossy().into_owned());
        }
        let mut without_hosts = configuration.clone();
        without_hosts.extension_hosts.clear();

        Ok(Self { extension_hosts, ..Self::at(&without_hosts, &root, &folder) })
    }

    /// The compile of the project at `root` with `configuration`, which enables no host, into `folder`.
    fn at(configuration: &Configuration, root: &Path, folder: &Path) -> Self {
        Self {
            configuration: configuration.clone(),
            extension_hosts: BTreeMap::new(),
            enabled: true,
            root: root.to_path_buf(),
            folder: folder.to_path_buf(),
            last: None,
        }
    }

    /// The project's hosts, each enabled one given the folder through [`HOST_ROOT`] and [`HOST_COMPILED`].
    pub(crate) fn extension_hosts(&self) -> &BTreeMap<String, ExtensionHostConfiguration> {
        &self.extension_hosts
    }

    /// Compiles the project for its hosts again after the files `changed` names changed, as `database` now holds them:
    /// from the last compile's analysis and record, when this process compiled last and no other run compiled into the
    /// folder since, or else as [`compile_for_hosts`] does. Returns what changed in the folder's tree since this process
    /// last held it.
    ///
    /// # Errors
    ///
    /// Returns the error that stops the compile.
    pub(crate) fn recompile(&mut self, database: &Database<'_>, changed: &[FileId]) -> Result<HostsChange, Error> {
        if !self.enabled {
            return Ok(HostsChange::Unchanged);
        }

        silenced(|| match self.last.take() {
            Some(last) => self.compile_changes(last, database, changed),
            None => self.compile_whole(true),
        })
    }

    /// Compiles the whole project under the lock of the folder's [`COMPILE_LOCK`], as [`compile_for_hosts`] says, and
    /// keeps the shared lock in [`HOSTS_FOLDERS`]. A project with no PHP# file gives its hosts nothing to read, so it
    /// takes no lock. When `keeping`, as a watch's compile is, it keeps what the next compile starts from, its analysis
    /// included, which a folder that was current needs the analysis of the project for. Returns what changed in the
    /// folder's tree since this process last held it: the files this compile changed, or any, when its record differs
    /// from the one the process last held, as after another run's compile between a watch's analyses. A folder that
    /// keeps no record can't tell, so any file may have changed.
    fn compile_whole(&mut self, keeping: bool) -> Result<HostsChange, Error> {
        self.last = None;
        let mut filesystems = Filesystems::new();
        let orchestrator = whole_workspace(&self.configuration, ColorChoice::Never, false)?;
        let mut listing = orchestrator.list_database(&self.configuration.source.workspace)?;
        let mut sources = source_files(&self.root, &listing, &mut filesystems, orchestrator.autoloader().as_deref());
        if !sources.keys().any(|name| name.ends_with(".sharp")) {
            return Ok(HostsChange::Unchanged);
        }

        let (lock, held, exclusive, stale) = self.lock(&mut listing, &mut sources, &mut filesystems, &orchestrator)?;
        let (configuration, root, folder) = (&self.configuration, &self.root, &self.folder);
        let compiled = if stale {
            Some(compile_into(configuration, root, folder, &orchestrator, &listing, &sources, filesystems.clone())?)
        } else {
            None
        };
        let (compiled, last) = match compiled {
            Some((compiled, last)) => (Some(compiled), keeping.then_some(last)),
            None if keeping => {
                let outputs = Record::read(&folder.join(INPUTS)).map(|mut record| std::mem::take(&mut record.outputs));
                let last = LastCompile::analyzed(
                    configuration,
                    root,
                    folder,
                    &orchestrator,
                    &listing,
                    &sources,
                    filesystems,
                    outputs.unwrap_or_default(),
                )?;
                (None, Some(last))
            }
            None => (None, None),
        };
        self.last = last;

        self.hold(lock, held, exclusive, compiled)
    }

    /// Takes the lock of the folder's [`COMPILE_LOCK`] for a compile of the whole project, as
    /// [`compile_for_hosts`] says: shared while the folder holds the compile of the project whose source files are
    /// `sources`, as [`source_files`] found them in `listing` with `filesystems`, and else exclusive. A run that waited
    /// for another's compile lists the project again with `orchestrator`, finds the sources again and checks the folder
    /// again. Gives the lock, what this process held of the folder, whether the lock is exclusive, and whether the
    /// folder needs the compile.
    ///
    /// # Errors
    ///
    /// Returns the error of taking the lock or of listing the project again.
    fn lock(
        &self,
        listing: &mut Listing,
        sources: &mut BTreeMap<String, Found>,
        filesystems: &mut Filesystems,
        orchestrator: &Orchestrator<'_>,
    ) -> Result<(std::fs::File, Option<HeldFolder>, bool, bool), Error> {
        let Self { configuration, root, folder, .. } = self;
        let path = folder.join(COMPILE_LOCK);
        let record_path = folder.join(INPUTS);
        let mut held = HOSTS_FOLDERS.lock().unwrap_or_else(PoisonError::into_inner).remove(folder);
        let lock = match held.as_mut().and_then(|held| held.lock.take()) {
            Some(lock) => lock,
            None => mago_database::lock::shared(&path)?,
        };
        if every_compiled_file_is_current(configuration, root, folder, sources, filesystems) {
            return Ok((lock, held, false, false));
        }
        let stale = std::fs::read(&record_path).ok();
        lock.unlock()
            .and_then(|()| lock.lock())
            .map_err(|error| DatabaseError::io(IOAction::WriteFile, &path, error))?;
        // Only a compile writes the record, under the exclusive lock, so a record that changed while this run waited
        // for the lock is another run's compile, whose tree this run checks again and reuses when it is current.
        let reused = std::fs::read(&record_path).ok() != stale && {
            *filesystems = Filesystems::new();
            *listing = orchestrator.list_database(&configuration.source.workspace)?;
            *sources = source_files(root, listing, filesystems, orchestrator.autoloader().as_deref());
            every_compiled_file_is_current(configuration, root, folder, sources, filesystems)
        };

        Ok((lock, held, true, !reused))
    }

    /// Lets `lock`, the lock [`Self::lock`] took, go back to shared when it is `exclusive`, and keeps it in
    /// [`HOSTS_FOLDERS`] while the process's hosts read the folder. Returns what changed in the folder's tree since this
    /// process last held it, as `held` holds that: the files `compiled` names, when this run compiled and a file in the
    /// folder changed, or any, when the folder's record differs from the one the process held.
    ///
    /// # Errors
    ///
    /// Returns the error of taking the shared lock.
    fn hold(
        &self,
        lock: std::fs::File,
        held: Option<HeldFolder>,
        exclusive: bool,
        compiled: Option<Option<Recompiled>>,
    ) -> Result<HostsChange, Error> {
        let path = self.folder.join(COMPILE_LOCK);
        if exclusive {
            lock.unlock()
                .and_then(|()| lock.lock_shared())
                .map_err(|error| DatabaseError::io(IOAction::WriteFile, &path, error))?;
        }
        let record = std::fs::read(self.folder.join(INPUTS)).ok().map(|bytes| unit::source_hash(&bytes));
        let last = held.map(|held| held.record);
        HOSTS_FOLDERS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.folder.clone(), HeldFolder { lock: Some(lock), record });

        Ok(match (compiled.flatten(), last) {
            (Some(recompiled), _) => HostsChange::Recompiled(recompiled),
            (None, Some(last)) if record.is_none() || last != record => HostsChange::Unknown,
            (None, _) => HostsChange::Unchanged,
        })
    }

    /// Compiles the project again from `last`, what the last compile of this process left, after the files `changed`
    /// names changed, as `database` holds them now, under the lock of the folder's [`COMPILE_LOCK`] as
    /// [`Self::compile_whole`] takes it. A run that compiled into the folder since this process held it last leaves
    /// `last` out of date, so the whole compile runs instead. Returns what changed in the folder's tree.
    fn compile_changes(
        &mut self,
        mut last: LastCompile,
        database: &Database<'_>,
        changed: &[FileId],
    ) -> Result<HostsChange, Error> {
        let path = self.folder.join(COMPILE_LOCK);
        let record_path = self.folder.join(INPUTS);
        let locking = |error| DatabaseError::io(IOAction::WriteFile, &path, error);
        let recorded = || std::fs::read(&record_path).ok().map(|bytes| unit::source_hash(&bytes));
        let hold = |lock: std::fs::File, record: Option<[u8; 16]>| {
            HOSTS_FOLDERS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(self.folder.clone(), HeldFolder { lock: Some(lock), record });
        };
        let Some(held) = HOSTS_FOLDERS.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.folder) else {
            return self.compile_whole(true);
        };
        let lock = match held.lock {
            Some(lock) => lock,
            None => mago_database::lock::shared(&path)?,
        };
        // Only a compile writes the record, under the exclusive lock, so a record other than the one this process held
        // last is another run's compile, which leaves `last` out of date. The whole compile then tells what changed.
        if recorded() != held.record {
            hold(lock, held.record);

            return self.compile_whole(true);
        }

        let touched = last.take_in(database, changed, &self.root);
        if touched.is_empty() {
            hold(lock, held.record);
            self.last = Some(last);

            return Ok(HostsChange::Unchanged);
        }
        lock.unlock().and_then(|()| lock.lock()).map_err(locking)?;
        if recorded() != held.record {
            lock.unlock().and_then(|()| lock.lock_shared()).map_err(locking)?;
            hold(lock, held.record);

            return self.compile_whole(true);
        }
        last.server.analyze_incremental(&touched)?;
        // A record the compile can't delete is in a folder it then fails to write its compiled files to.
        let _ = std::fs::remove_file(&record_path);
        let record = Record::start(
            &self.configuration,
            &self.root,
            &last.sources,
            &self.folder,
            last.filesystems.clone(),
            last.autoloader.as_deref(),
        );
        let compiled = write_hosts_folder(&self.configuration, &self.root, &self.folder, &mut last, record)?;
        lock.unlock().and_then(|()| lock.lock_shared()).map_err(locking)?;
        hold(lock, recorded());
        self.last = Some(last);

        Ok(compiled.map_or(HostsChange::Unchanged, HostsChange::Recompiled))
    }
}

/// Compiles the project at `root`, whose source files are `sources`, as [`source_files`] found them in `listing`, which
/// `orchestrator` listed, with `filesystems`, into `folder` for its extension hosts, as [`compile_for_hosts`] says, from
/// an analysis of its own, as [`write_hosts_folder`] writes it. Returns, when a file in `folder` changed, the files
/// whose compiled file changed, and what the compile leaves for the next one to start from.
fn compile_into(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    orchestrator: &Orchestrator<'_>,
    listing: &Listing,
    sources: &BTreeMap<String, Found>,
    filesystems: Filesystems,
) -> Result<(Option<Recompiled>, LastCompile), Error> {
    let record_path = folder.join(INPUTS);
    // The last compile's outputs, which it keeps unread while their status holds.
    let outputs = Record::read(&record_path).map(|mut record| std::mem::take(&mut record.outputs)).unwrap_or_default();
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);
    let record =
        Record::start(configuration, root, sources, folder, filesystems.clone(), orchestrator.autoloader().as_deref());
    let mut last =
        LastCompile::analyzed(configuration, root, folder, orchestrator, listing, sources, filesystems, outputs)?;
    let compiled = write_hosts_folder(configuration, root, folder, &mut last, record)?;

    Ok((compiled, last))
}

impl LastCompile {
    /// Takes in that the files `changed` names changed, as `database` names them now: each the analysis's database
    /// holds, or the project's would hold at its path, goes in or out of the analysis's database and of the sources the
    /// record keeps, and its stamp and those of the listings of its folder and each folder above it are forgotten. Each
    /// file's bytes are read after its status, as [`source_files`] and the loader take them, so a write between the two
    /// leaves a status the record never matches. Returns the files the analysis holds that changed.
    fn take_in(&mut self, database: &Database<'_>, changed: &[FileId], root: &Path) -> Vec<FileId> {
        let mut touched = Vec::new();
        for id in changed {
            let held = self.server.database().get(id).ok();
            let Some(file) = database
                .get(id)
                .ok()
                .filter(|file| held.is_some() || file.path.as_deref().is_some_and(|path| self.workspace.contains(path)))
                .or(held)
            else {
                continue;
            };
            let name = String::from_utf8_lossy(&file.name).into_owned();
            let path = root.join(&name);
            let status = std::fs::metadata(&path)
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|metadata| Status::of(&path, &metadata, &mut self.filesystems));
            match (status, read_listed(&path)) {
                (Some(status), Ok(Some(contents))) => {
                    let file_type = match file.file_type {
                        FileType::Vendored if Dialect::of(&file).is_sharp() => FileType::Host,
                        file_type => file_type,
                    };
                    self.server.database_mut().add(copy_of(&file, file_type, Cow::Owned(contents)));
                    if !self.autoloader.as_ref().is_some_and(|autoloader| autoloader.serves(&path)) {
                        let real =
                            path.canonicalize().ok().map(|real| source_name(root, &real)).filter(|real| *real != name);
                        self.sources.insert(name.clone(), Found(status, real));
                    }
                }
                _ => {
                    self.server.database_mut().delete(*id);
                    self.sources.remove(&name);
                }
            }
            touched.push(*id);
            let name = name.into_bytes();
            self.stamps.remove(&name);
            for (end, _) in name.iter().enumerate().filter(|(_, byte)| **byte == b'/') {
                self.stamps.remove(&[&name[..=end], unit::SOURCE_LISTING.as_bytes()].concat());
            }
            self.stamps.remove(unit::SOURCE_LISTING.as_bytes());
        }

        touched
    }

    /// What a compile into `folder` for the hosts of the project at `root` starts from: the analysis of the project
    /// without the hosts, of the files `listing` names, which `orchestrator` listed, whose source files are `sources`, as
    /// [`source_files`] found them with `filesystems`, the record of the compiled files the folder holds, and
    /// `outputs`, the status and hash of each file the folder's record names as written.
    #[allow(clippy::too_many_arguments)]
    fn analyzed(
        configuration: &Configuration,
        root: &Path,
        folder: &Path,
        orchestrator: &Orchestrator<'_>,
        listing: &Listing,
        sources: &BTreeMap<String, Found>,
        filesystems: Filesystems,
        outputs: Outputs,
    ) -> Result<Self, Error> {
        let (database, _) =
            with_packages(orchestrator.load_listed(&configuration.source.workspace, listing)?.into_static());
        let library = standard_library(root, &database).map_err(OrchestratorError::General)?;
        let mut server = Server::new(database.into_static(), decode_prelude, server_settings(orchestrator))
            .keeping_lowering_inputs();
        server.analyze()?;

        Ok(Self {
            server,
            autoloader: orchestrator.autoloader(),
            workspace: WorkspaceMatcher::from_configuration(
                &orchestrator.database_configuration(&configuration.source.workspace, true),
            )?,
            sources: sources.clone(),
            filesystems,
            library,
            compiled_files: CompiledFiles::decode(
                &std::fs::read(folder.join(COMPILED_FILES)).unwrap_or_default(),
                BUILD_ID,
                configuration_hash(configuration),
            ),
            outputs,
            stamps: HashMap::new(),
        })
    }
}

/// Compiles the project at `root` from `last`'s analysis into `folder` for its extension hosts, as
/// [`compile_for_hosts`] says, and keeps the record of what it read and wrote in `folder`'s [`INPUTS`], which `record`
/// starts. Each compiled file whose status the last compile recorded holds is kept unread while its key and its inputs'
/// stamps hold, and each other one is written only when its bytes changed. Its log never decides the record: the
/// loader and the analysis log from their worker threads too, which the main thread's subscriber doesn't silence, and
/// the run with the hosts that follows reports what they log. Returns, when a file in `folder` changed, the files
/// whose compiled file changed, and leaves in `last` what the next compile starts from.
fn write_hosts_folder(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    last: &mut LastCompile,
    record: Option<Record>,
) -> Result<Option<Recompiled>, Error> {
    let LastCompile {
        server,
        autoloader,
        sources,
        filesystems,
        library,
        compiled_files: last_compiled,
        outputs,
        stamps,
        ..
    } = last;
    // A file whose errors the lowering does not survive is refused, whatever panic message its lowering printed.
    let printing = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic| tracing::debug!("{panic}")));
    let stamped = Mutex::new(Vec::new());
    let fresh = Mutex::new(Vec::new());
    let held = server.database().read_only();
    // A source file's stamp holds until the file changes, and a listing's until a file of its folder does.
    let remembered = |path: &[u8]| -> Result<Option<unit::Input>, DatabaseError> {
        if let Some(input) = stamps.get(path) {
            return Ok(Some(input.clone()));
        }
        let input = stamp_of(root, &held, path)?;
        if let Some(input) = &input
            && (held.get_id(path).is_some() || path.ends_with(unit::SOURCE_LISTING.as_bytes()))
        {
            fresh.lock().unwrap_or_else(PoisonError::into_inner).push(input.clone());
        }

        Ok(input)
    };
    let compilations = {
        let (compiled, mut intact, write) = (
            host_compiled(root, folder, sources),
            host_intact(root, folder, sources, outputs, filesystems),
            host_write(root, folder, sources),
        );
        server.compile(
            Folder { last: last_compiled, compiled: &compiled, intact: &mut intact, write: &write },
            None,
            |path| {
                let input = remembered(path)?;
                stamped.lock().unwrap_or_else(PoisonError::into_inner).push(recorded(path, input.as_ref()));

                Ok(input)
            },
            |_, _, _| Ok(Default::default()),
            BUILD_ID,
            library,
            |_| IssueCollection::default(),
        )
    };
    std::panic::set_hook(printing);
    let compilations = compilations?.0;
    let inputs = listener_inputs(server.database(), remembered)?;
    let (recompiled, next_outputs, compiled_files) = write_hosts(
        configuration,
        root,
        folder,
        sources,
        server,
        autoloader.as_deref(),
        compilations,
        outputs,
        in_path_order(stamped),
        &inputs,
        record,
    )?;
    *outputs = next_outputs;
    *last_compiled = compiled_files;
    stamps.extend(fresh.into_inner().unwrap_or_else(PoisonError::into_inner).into_iter().map(|input| {
        let path = input.path.clone();
        (path, input)
    }));

    Ok(recompiled)
}

/// Writes into `folder`, for the extension hosts of the project at `root` with `configuration`, the folder's files
/// beside the compiled files of `compiled`, which a compile made from `server`'s analysis and wrote as [`host_write`]
/// writes them, and keeps the record of
/// what the compile read and wrote in `folder`'s [`INPUTS`], which `record` starts, with `stamped`, the stamps the
/// compile took, those of the files `autoloader` serves, and `inputs`, the listeners' inputs [`listener_inputs`] gave
/// from `server`'s database. A compiled file
/// the compile kept keeps the status and hash `outputs` holds for it. Returns, when a file in `folder` changed, the
/// files whose compiled file changed, then the status and hash of each file the compile wrote or kept, and what the
/// next compile keeps files by.
#[allow(clippy::too_many_arguments)]
fn write_hosts(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    sources: &BTreeMap<String, Found>,
    server: &Server,
    autoloader: Option<&Autoloader>,
    (compilations, compiled_files): Compiled<Result<Written, DatabaseError>>,
    outputs: &Outputs,
    mut stamped: Vec<Stamp>,
    inputs: &[unit::Input],
    mut record: Option<Record>,
) -> Result<(Option<Recompiled>, Outputs, CompiledFiles), Error> {
    let record_path = folder.join(INPUTS);
    let compiled_files_path = folder.join(COMPILED_FILES);
    let database = server.database();
    stamped.extend(served_stamps(root, database, autoloader)?);

    // Whether a file a host's worker loads changed, so a watch knows to start the workers again.
    let mut changed = false;
    let mut keep = |path: &Path, hash: [u8; 16], wrote: bool| {
        changed |= wrote;
        if let Some(record) = record.as_mut() {
            record.output(path, hash);
        }
    };
    let mut written = BTreeSet::new();
    let mut refused = IssueCollection::default();
    let mut accepted = HashSet::new();
    let mut recompiled = HashSet::new();
    let bytes = compiled_files.encode(BUILD_ID, configuration_hash(configuration))?;
    keep(&compiled_files_path, unit::source_hash(&bytes), write_if_changed(&compiled_files_path, &bytes)?);
    written.insert(compiled_files_path);
    let mut kept = Vec::new();
    for (file_id, compilation) in compilations {
        let file = database.get_ref(&file_id)?;
        let Some(compiled) = host_compiled_path(root, folder, sources, file)? else {
            continue;
        };
        match compilation {
            Compilation::Accepted(output) => {
                let Written { hash, wrote } = output?;
                if wrote {
                    recompiled.insert(file_id);
                }
                keep(&compiled, hash, wrote);
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Kept => {
                kept.push(compiled.clone());
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Refused(errors) => {
                if compiled.exists() {
                    recompiled.insert(file_id);
                }
                refused.extend(errors);
            }
        }
    }
    let classes = server
        .codebase()
        .class_likes
        .values()
        .filter(|class| recompiled.contains(&class.span.file_id))
        .map(|class| ascii_lowercase_word(class.original_name.as_bytes()))
        .collect();

    if !inputs.is_empty() {
        let listeners = server.listeners();
        let externs = server.extern_attributes();
        let externs: Vec<&ExternAttributes> = externs.iter().filter(|line| accepted.contains(&line.file_id)).collect();
        let families = server.families();
        let families: Vec<&Family> = families.iter().filter(|family| accepted.contains(&family.file_id)).collect();
        let accepted: Vec<&Listener> =
            listeners.iter().filter(|listener| accepted.contains(&listener.file_id)).collect();
        let effects = server.extern_effects();
        let table = folder.join(LISTENERS);
        let dispatcher = server.dispatcher_namespace();
        let modules = server.modules();
        let packages = package_folders(root);
        let bytes = listener_table(inputs, &accepted, dispatcher, &packages, &externs, &effects, &families, &modules);
        keep(&table, unit::source_hash(&bytes), write_if_changed(&table, &bytes)?);
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
            tool_version: VERSION,
        },
    )
    .report(refused, None)?;
    let report = report.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let path = folder.join(HOST_REPORT);
    keep(&path, unit::source_hash(&report), write_if_changed(&path, &report)?);
    written.insert(path);
    if let Some(record) = record.as_mut() {
        for compiled in kept {
            if let Some(output) = outputs.get(&source_name(root, &compiled)) {
                record.unchanged_output(&compiled, *output);
            }
        }
    }
    // The statuses of the files this compile wrote or kept, which the next one keeps unread while they hold.
    let next_outputs = record.as_ref().map(|record| record.outputs.clone()).unwrap_or_default();
    // A compile with a file that doesn't lower keeps no record, so the next one tries that file again.
    let record = record.filter(|_| lowered);
    written.extend(record.as_ref().and_then(Record::marker_path));
    written.insert(folder.join(COMPILE_LOCK));
    let deleted = delete_all_but(folder, &written, &folder.join(PACKAGE_FOLDER))?;
    if let Some(record) = record {
        let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
        record.write(&record_path, Outcome::default(), database, stamped.into_iter().chain(listed))?;
    }

    Ok(((changed || deleted > 0).then_some(Recompiled { files: recompiled, classes }), next_outputs, compiled_files))
}

/// The compiled file of a file in `folder`, the hosts' folder of the project at `root` whose source files are
/// `sources`, or none when no file is there.
fn host_compiled<'a>(
    root: &'a Path,
    folder: &'a Path,
    sources: &'a BTreeMap<String, Found>,
) -> impl Fn(&File) -> Result<Option<Vec<u8>>, DatabaseError> + Sync + 'a {
    move |file: &File| match host_compiled_path(root, folder, sources, file)? {
        Some(compiled) => read_listed(&compiled),
        None => Ok(None),
    }
}

/// Writes the compiled file of a file in `folder`, the hosts' folder of the project at `root` whose source files are
/// `sources`, as [`write_compiled`] does.
fn host_write<'a>(
    root: &'a Path,
    folder: &'a Path,
    sources: &'a BTreeMap<String, Found>,
) -> impl Fn(&File, Vec<u8>) -> Result<Written, DatabaseError> + Sync + 'a {
    move |file: &File, bytes: Vec<u8>| write_compiled(host_compiled_path(root, folder, sources, file)?, &bytes)
}

/// Whether the compiled file of a file in `folder`, the hosts' folder of the project at `root` whose source files are
/// `sources`, is the file the last compile wrote, by the status `outputs` holds for it and `filesystems`.
fn host_intact<'a>(
    root: &'a Path,
    folder: &'a Path,
    sources: &'a BTreeMap<String, Found>,
    outputs: &'a Outputs,
    filesystems: &'a mut Filesystems,
) -> impl FnMut(&File) -> bool + 'a {
    move |file: &File| match host_compiled_path(root, folder, sources, file) {
        Ok(Some(compiled)) => is_unchanged(&compiled, outputs.get(&source_name(root, &compiled)), filesystems),
        _ => false,
    }
}

/// Where the hosts' compile of the project at `root`, whose source files are `sources`, into `folder` puts the
/// compiled file of `file`: where [`compiled_path_of`] puts it in the `.sharp` folder, in `folder` instead, or none
/// when `file` is a link outside `root`.
fn host_compiled_path(
    root: &Path,
    folder: &Path,
    sources: &BTreeMap<String, Found>,
    file: &File,
) -> Result<Option<PathBuf>, DatabaseError> {
    Ok(compiled_path_of(root, sources, file)?.and_then(|compiled| in_hosts_folder(root, folder, &compiled)))
}

/// Where the hosts' `folder` of the project at `root` keeps the compiled file the project's `.sharp` folder keeps at
/// `compiled`, or none when `compiled` is outside that folder.
fn in_hosts_folder(root: &Path, folder: &Path, compiled: &Path) -> Option<PathBuf> {
    compiled.strip_prefix(root.join(unit::COMPILED_FOLDER)).ok().map(|relative| folder.join(relative))
}

/// What the extension hosts of the project at `root` load as their workers start, before the compile reads what they
/// register, as [`hosts_reach`] finds it: files of the project's database, and the folder of each enabled host by its
/// name, from the project's root and ending in `/`.
struct HostsReach {
    root: PathBuf,
    files: HashSet<FileId>,
    folders: BTreeMap<String, String>,
}

impl HostsReach {
    /// `error`, the hosts' failure to start, as the error of the class a host loaded from outside this reach when the
    /// engine refused that class's file, of `database`: the `CompileError` a host's file threw in that file, which the
    /// compile writes for the hosts only once they started. Any other error stays.
    fn refusing(&self, database: &Database<'_>, error: PluginError) -> PluginError {
        let PluginError::External(external) = &error else {
            return error;
        };
        let ExternalAnalyzerError::Worker(WorkerError::Load { host, thrown: Some(thrown), .. }) = external.as_ref()
        else {
            return error;
        };
        let Some(folder) = self.folders.get(host).filter(|_| thrown.class == "CompileError") else {
            return error;
        };
        let refused = File::logical_name(&self.root, &thrown.file);
        let outside =
            database.files().find(|file| *file.name == *refused && is_lowered(file) && !self.files.contains(&file.id));
        let Some(outside) = outside else {
            return error;
        };
        let name = String::from_utf8_lossy(&outside.name);
        let stem = Path::new(name.as_ref()).file_stem().unwrap_or_default().to_string_lossy();
        let class = match &outside.folder_namespace {
            Some(FolderNamespace::Namespace(namespace)) => format!("{namespace}.{stem}"),
            _ => stem.into_owned(),
        };

        PluginError::External(Arc::new(ExternalAnalyzerError::Worker(WorkerError::Load {
            host: host.clone(),
            message: format!(
                "`{class}` isn't in extension host `{host}`'s folder `{folder}` or named from it, so it isn't compiled \
                 before the host starts. Move it under `{folder}` or name it from the host's code."
            ),
            thrown: Some(thrown.clone()),
        })))
    }
}

/// What the extension hosts of `hosts` load as their workers start among the files of the project's `database` at
/// `root`: the standard library's files, each enabled host's file and every PHP# file under the folder that holds it, as
/// a package is its folder, every file of the project those files name, and every file those name in turn. A file's
/// names are those its scan names, every name its code resolves and each class-like a `@var` tag of its bodies names,
/// as an analysis's scan finds the class-likes a file reaches. A name reaches the file its namespace's folder holds
/// under its last part, as a type of PHP# and a class Composer loads by PSR-4 each live in one, so the walk parses only
/// the files it reaches, and the standard library, which names only itself, not at all.
fn hosts_reach(
    configuration: &Configuration,
    root: &Path,
    hosts: &HostsCompile,
    database: &Database<'_>,
) -> HostsReach {
    let parser = configuration.parser.to_settings();
    let files: Vec<Arc<File>> = database.files().filter(|file| file.file_type == FileType::Host).collect();
    let declared = |file: &File| match &file.folder_namespace {
        Some(FolderNamespace::Namespace(namespace)) => {
            let name = String::from_utf8_lossy(&file.name);
            let stem = Path::new(name.as_ref()).file_stem()?.to_string_lossy().into_owned();

            Some(format!("{}\\{stem}", namespace.replace('.', "\\")))
        }
        _ => None,
    };
    let classes = NamespaceClasses::new(
        files.iter().filter(|file| Dialect::of(file).is_sharp()).filter_map(|file| Some((declared(file)?, true))),
    );
    let mut declaring: HashMap<Word, usize> = HashMap::default();
    for (index, file) in files.iter().enumerate() {
        if let Some(name) = declared(file) {
            declaring.insert(ascii_lowercase_word(name.as_bytes()), index);
        }
    }
    let scan = |file: &File| {
        let arena = LocalArena::new();
        let program = parse_file_with_settings(&arena, file, parser);
        let names = NameResolver::new(&arena).resolve_in(
            program,
            &classes,
            &ChangingMembers::default(),
            &ExternNamespaces::default(),
        );
        let metadata = scan_program(&arena, file, program, &names, configuration.php_version);
        let mut reached: Vec<Word> = metadata.named_class_likes().into_iter().collect();
        reached.extend(names.iter().map(|(_, _, name, _)| mago_word::word(name)));
        reached.extend(var_tag_class_likes(&arena, program));

        reached
    };

    let mut taken = vec![false; files.len()];
    let mut reach = HashSet::default();
    for (index, file) in files.iter().enumerate().filter(|(_, file)| file.is_standard_library) {
        taken[index] = true;
        reach.insert(file.id);
    }
    let mut folders = BTreeMap::new();
    let mut wave: Vec<usize> = Vec::new();
    let mut names: Vec<Word> = Vec::new();
    for (name, host) in hosts.extension_hosts.iter().filter(|(_, host)| host.enabled) {
        let path = host.host_file(name).file;
        let path = path.canonicalize().unwrap_or(path);
        let folder = path.parent().and_then(|folder| folder.strip_prefix(root).ok()).map(|folder| {
            let folder = folder.to_string_lossy();
            if folder.is_empty() { String::new() } else { format!("{folder}/") }
        });
        if let Some(folder) = folder {
            for (index, file) in files.iter().enumerate() {
                if is_lowered(file)
                    && file.name.starts_with(folder.as_bytes())
                    && !std::mem::replace(&mut taken[index], true)
                {
                    wave.push(index);
                }
            }
            folders.insert(name.clone(), folder);
        }
        if let Ok(contents) = std::fs::read(&path) {
            let name = Cow::Owned(File::logical_name(root, &path));
            names.extend(scan(&File::new(name, FileType::Host, Some(path), contents.into())));
        }
    }
    loop {
        for name in names.drain(..) {
            if let Some(&index) = declaring.get(&ascii_lowercase_word(name.as_bytes()))
                && !std::mem::replace(&mut taken[index], true)
            {
                wave.push(index);
            }
        }
        if wave.is_empty() {
            break;
        }
        reach.extend(wave.iter().map(|&index| files[index].id));
        names = wave.par_iter().flat_map_iter(|&index| scan(&files[index])).collect();
        wave.clear();
    }

    HostsReach { root: root.to_path_buf(), files: reach, folders }
}

/// Whether the hosts' `folder` of the project at `root`, whose source files are `sources`, holds for each PHP# file of
/// `database` among `files` a compiled file the engine runs: one this checker build wrote from the file as it is now,
/// none of whose inputs changed since.
fn holds_compiled(
    root: &Path,
    folder: &Path,
    sources: &BTreeMap<String, Found>,
    database: &Database<'_>,
    files: &HashSet<FileId>,
) -> bool {
    let lowered: Vec<Arc<File>> =
        database.files().filter(|file| files.contains(&file.id) && is_lowered(file)).collect();

    lowered.par_iter().all(|file| {
        let Ok(Some(compiled)) = host_compiled_path(root, folder, sources, file) else {
            return false;
        };
        let Ok(Some(bytes)) = read_listed(&compiled) else {
            return false;
        };

        unit::header(&bytes)
            .is_ok_and(|header| header.checker == BUILD_ID && header.source_hash == unit::source_hash(&file.contents))
            && unit::inputs_changed(root, &bytes).is_ok_and(|changed| !changed)
    })
}

/// Lowers what the extension hosts of `hosts` load as their workers start, [`hosts_reach`], alone into their folder of
/// the project at `root`, whose source files are `sources`, against `library`, unless the folder holds a current
/// compiled file of each of those files already: the compiled file of each of those files of `database`, the project's
/// database as [`with_packages`] gives it, written only when its bytes changed, and no other file, so the compile of the
/// whole project that follows writes the rest of the folder and its record. Those files name no file outside them, so
/// their analysis holds them and Composer's packages alone, and runs as the hosts' compile does, without the hosts, on
/// an orchestrator of its own, since the compile's orchestrator records what the hosts are asked. Returns that reach.
///
/// # Errors
///
/// Returns the error of analyzing those files or of writing a compiled file.
fn write_hosts_reach(
    configuration: &Configuration,
    root: &Path,
    hosts: &HostsCompile,
    sources: &BTreeMap<String, Found>,
    database: &Database<'static>,
    library: &unit::Library,
) -> Result<HostsReach, Error> {
    let reach = hosts_reach(configuration, root, hosts, database);
    if holds_compiled(root, &hosts.folder, sources, database, &reach.files) {
        return Ok(reach);
    }
    let mut alone = database.clone();
    let outside: Vec<FileId> = alone
        .files()
        .filter(|file| file.file_type == FileType::Host && !reach.files.contains(&file.id))
        .map(|file| file.id)
        .collect();
    for id in outside {
        alone.delete(id);
    }
    let orchestrator = whole_workspace(configuration, ColorChoice::Never, false)?;
    let mut server = Server::new(alone, decode_prelude, server_settings(&orchestrator)).keeping_lowering_inputs();
    server.analyze()?;
    let held = server.database().read_only();
    let folder = &hosts.folder;
    let (compiled, write) = (host_compiled(root, folder, sources), host_write(root, folder, sources));
    // A file whose errors the lowering does not survive is refused, whatever panic message its lowering printed, and the
    // compile of the whole project reports it.
    let printing = std::panic::take_hook();
    std::panic::set_hook(Box::new(|panic| tracing::debug!("{panic}")));
    let compilations = server.compile(
        Folder { last: &CompiledFiles::default(), compiled: &compiled, intact: &mut |_: &File| false, write: &write },
        None,
        |path| stamp_of(root, &held, path),
        |_, _, _| Ok(Default::default()),
        BUILD_ID,
        library,
        |_| IssueCollection::default(),
    );
    std::panic::set_hook(printing);
    let ((compilations, _), _) = compilations?;
    for (_, compilation) in compilations {
        if let Compilation::Accepted(written) = compilation {
            written?;
        }
    }

    Ok(reach)
}

/// The project's database: every file the analyzer loads with the prelude, and each `.sharp` file of a package as a
/// file of the project, which the compile checks too. Also gives the IDs of those packaged files.
pub(crate) fn project_database(
    orchestrator: &Orchestrator<'_>,
    workspace: &Path,
) -> Result<(Database<'static>, HashSet<FileId>), Error> {
    Ok(with_packages(orchestrator.load_database(workspace, true, None, None)?.into_static()))
}

/// The project's database, as [`project_database`] gives it, from `database`, every file the analyzer loads.
fn with_packages(mut database: Database<'static>) -> (Database<'static>, HashSet<FileId>) {
    database.merge_base(Prelude::decode_database(PRELUDE_BYTES).expect("Failed to decode embedded prelude"));
    let packaged: Vec<File> = database
        .files()
        .filter(|file| file.file_type == FileType::Vendored && Dialect::of(file).is_sharp())
        .map(|file| File { modified_ns: file.modified_ns, ..copy_of(&file, FileType::Host, file.contents.clone()) })
        .collect();
    let included: HashSet<FileId> = packaged.iter().map(|file| file.id).collect();
    for file in packaged {
        database.add(file);
    }

    (database, included)
}

/// Each file of `included`, the `.sharp` files [`with_packages`] copied into `database`, that is a Composer package's,
/// whose extension host issues an analysis leaves out.
fn package_files(database: &Database<'_>, included: &HashSet<FileId>) -> Vec<FileId> {
    database
        .files()
        .filter(|file| included.contains(&file.id) && package_of(&file.name).is_some())
        .map(|file| file.id)
        .collect()
}

/// A copy of `file` as a file of `file_type` that holds `contents`, as a package's `.sharp` file is copied into a file
/// of the project, which the compile checks too.
fn copy_of(file: &File, file_type: FileType, contents: Cow<'static, [u8]>) -> File {
    let mut copy = File::new(file.name.clone(), file_type, file.path.clone(), contents);
    copy.is_standard_library = file.is_standard_library;
    copy.folder_namespace = file.folder_namespace.clone();
    copy.is_project = file.is_project;
    copy
}

/// A fresh orchestrator of the whole workspace, as `mago analyze` makes one when no path is named, with Composer's
/// autoload maps read, which drawing progress bars when `use_progress_bars` is set. It starts no extension host.
///
/// # Errors
///
/// Returns the error of reading Composer's autoload maps.
pub(crate) fn whole_workspace(
    configuration: &Configuration,
    color_choice: ColorChoice,
    use_progress_bars: bool,
) -> Result<Orchestrator<'_>, Error> {
    let mut orchestrator = create_orchestrator(configuration, color_choice, false, use_progress_bars, false);
    analyze_the_whole_workspace_without_paths(&mut orchestrator);
    orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
    orchestrator.autoload_vendor(&configuration.source.workspace)?;

    Ok(orchestrator)
}

/// The stamp of each file of `database` that `autoloader`'s maps served the analysis, which the record keeps as an
/// input, as [`source_files`] leaves out every file the maps serve.
///
/// # Errors
///
/// Returns the error of reading a file.
fn served_stamps(
    root: &Path,
    database: &(impl DatabaseReader + Sync),
    autoloader: Option<&Autoloader>,
) -> Result<Vec<Stamp>, Error> {
    let Some(autoloader) = autoloader else {
        return Ok(Vec::new());
    };

    let served: Vec<_> = database
        .files()
        .filter(|file| !file.name.starts_with(VENDOR))
        .filter(|file| file.path.as_deref().is_some_and(|path| autoloader.serves(path)))
        .collect();

    // Stamped at once, and the first file in order that can't be read is the one named, as one after another.
    let stamped: Vec<Result<Stamp, Error>> = served
        .par_iter()
        .map(|file| Ok(recorded(&file.name, stamp_of(root, database, &file.name)?.as_ref())))
        .collect();

    stamped.into_iter().collect()
}

/// The path of `file`, a file of the project at `root`: its own, or its name under `root`.
fn source_path(root: &Path, file: &File) -> PathBuf {
    file.path.clone().unwrap_or_else(|| root.join(String::from_utf8_lossy(&file.name).as_ref()))
}

/// Where the compiled file of `file`, a file of the project at `root`, lives in its `.sharp` folder, as
/// [`unit::compiled_path`] places it: at the real path the walk that found the file resolved, which `sources` holds,
/// and at the real path the disk resolves for a file the walk did not list.
///
/// # Errors
///
/// Returns the error of resolving the real path of a file the walk did not list.
fn compiled_path_of(
    root: &Path,
    sources: &BTreeMap<String, Found>,
    file: &File,
) -> Result<Option<PathBuf>, DatabaseError> {
    let name = String::from_utf8_lossy(&file.name);
    match sources.get(name.as_ref()) {
        Some(Found(_, real)) => Ok(unit::compiled_path_of_real(root, &root.join(real.as_deref().unwrap_or(&name)))),
        None => unit::compiled_path(root, &source_path(root, file)),
    }
}

/// Writes `bytes` to the file at `path`, unless it holds them already, so a compile that changes nothing in it leaves
/// it as it was. Returns whether it wrote.
///
/// # Errors
///
/// Returns the error of writing the file.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, DatabaseError> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    write_by_rename(path, bytes)?;

    Ok(true)
}

/// What a compile did with the bytes of an accepted file's compiled file, as [`write_compiled`] wrote them.
#[derive(Debug)]
struct Written {
    /// The hash of the bytes, as the record keeps it.
    hash: [u8; 16],
    /// Whether the bytes differed from the file's, so the compile wrote them.
    wrote: bool,
}

/// Writes `bytes`, the compiled file of an accepted file, to `compiled`, its path, when the file has one, as
/// [`write_if_changed`] does.
///
/// # Errors
///
/// Returns the error of writing the file.
fn write_compiled(compiled: Option<PathBuf>, bytes: &[u8]) -> Result<Written, DatabaseError> {
    let wrote = match compiled {
        Some(compiled) => write_if_changed(&compiled, bytes)?,
        None => false,
    };

    Ok(Written { hash: unit::source_hash(bytes), wrote })
}

/// Whether the compile lowers `file`, a PHP# file of the project, which is the scope of its analysis. The compile
/// refuses a PHP# file for the errors reported in it, and a node-analysis hook reports only in the file it inspects, so
/// the hooks run only in these files, as they run only in the named files of `mago analyze <paths>`.
fn is_lowered(file: &File) -> bool {
    file.file_type == FileType::Host && Dialect::of(file).is_sharp()
}

/// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every other
/// file in the `.sharp` folder, as [`CompileCommand::execute`] says.
///
/// Spec section 27: the `.sharp` folder holds the one compile of a tree, from the analysis without the extension hosts,
/// as `mago --no-extensions compile` and a whole `mago analyze` write it, so the three keep each other's. With a host
/// enabled, the run compiles as a whole `mago analyze` does, the hosts' folder included, and the hosts then take its
/// analysis over: their rules only report, and each error they report in a PHP# file fails the run.
fn compile(configuration: &Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
    if !configuration.extension_hosts.values().any(|host| host.enabled) {
        let (outcome, database, ..) = compiled(configuration, color_choice, None, Keeping::Nothing)?;

        return outcome.print(database, configuration, color_choice);
    }

    let (started, outcome, loaded) = compile_with_hosts(configuration, color_choice, true)?;
    let (database, issues) = hosted(configuration, color_choice, started, loaded)?;
    let errors = issues
        .into_iter()
        .filter(|issue| issue.level == Level::Error && !outcome.issues.iter().any(|refused| refused == issue));
    let errors = Hidden::new(configuration)?.shown(IssueCollection::from(errors), &database);

    outcome.with_hosts_errors(errors).print(database, configuration, color_choice)
}

/// The issues the extension hosts `started` report in the PHP# files a compile lowers, the project's own, never a
/// Composer package's, with the database they point into, as the compile's own analysis with the hosts reported them:
/// the compile's analysis that `loaded` holds, scoped to those files, which the hosts take over as a whole `mago analyze`
/// does, [`Loaded::taken_over`], or else, when the compile had nothing to do, an analysis of the project with the hosts.
///
/// # Errors
///
/// Returns the error of loading the project or of analyzing it.
fn hosted(
    configuration: &Configuration,
    color_choice: ColorChoice,
    started: Option<Arc<ExternalAnalyzerHandle>>,
    loaded: Loaded,
) -> Result<(ReadDatabase, IssueCollection), Error> {
    let orchestrator = whole_workspace(configuration, color_choice, true)?;
    if let Some(started) = started {
        orchestrator.set_external_analyzer_handle(started);
    }
    let loaded = match loaded {
        Loaded::Hosted { server, requests } => {
            Loaded::Hosted { server: Box::new(server.scoped_to(is_lowered)), requests }
        }
        loaded => loaded,
    };
    let (server, issues) = match loaded.taken_over(&orchestrator)? {
        Loaded::Analysis(server, issues) => (*server, issues),
        Loaded::Nothing | Loaded::Files(..) | Loaded::Hosted { .. } => {
            let (database, included) = project_database(&orchestrator, &configuration.source.workspace)?;
            let package_files = package_files(&database, &included);
            let mut server = Server::new(database, decode_prelude, server_settings(&orchestrator))
                .with_package_files(package_files)
                .scoped_to(is_lowered);
            let issues = server.analyze()?.issues;

            (server, issues)
        }
    };

    Ok((server.database().read_only(), issues))
}

/// What the project's configuration hides of an analysis's issues, as `mago analyze` hides them: each issue an `ignore`
/// entry or the baseline targets.
struct Hidden {
    ignored: CompiledIgnoreSet,
    baseline: Option<Baseline>,
}

impl Hidden {
    /// What `configuration` hides.
    ///
    /// # Errors
    ///
    /// Returns the error of reading the baseline.
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

    /// `issues`, each in a file of `database`, without each one an `ignore` entry or the baseline hides.
    fn shown(&self, mut issues: IssueCollection, database: &ReadDatabase) -> IssueCollection {
        issues.filter_out_ignored(&self.ignored, |file_id| {
            database.get_ref(&file_id).ok().map(|file| String::from_utf8_lossy(&file.name).into_owned())
        });
        match &self.baseline {
            Some(baseline) => baseline.filter_issues(issues, database),
            None => issues,
        }
    }
}

/// What a whole `mago analyze` reports from, of what its compile loaded and analyzed.
pub(crate) enum Loaded {
    /// Nothing, when the compile had nothing to do: the run loads the files itself.
    Nothing,
    /// Every file the compile loaded, as `mago analyze` loads them with no extension host started, which the analysis
    /// with the hosts reads instead of loading them again, and the first scan of each file the compile's analysis
    /// scanned, which it takes for each file it holds as that analysis did instead of scanning it again.
    Files(Box<Database<'static>>, Scans),
    /// The compile's own analysis, which a project with no extension host enabled reports instead of analyzing again,
    /// and its issues. Its database holds each package's `.sharp` file as a file of the project, which the compile
    /// checks too, and it reports in every file but those, whose refusals the compile's outcome holds.
    Analysis(Box<Server>, IssueCollection),
    /// The compile's own analysis of a project with an enabled host, with the return-type requests it recorded, which
    /// the run takes over with the hosts.
    Hosted { server: Box<Server>, requests: ReturnTypeRequests },
}

impl Loaded {
    /// This, with the compile's own analysis taken over by the extension hosts of `orchestrator`, as a whole
    /// `mago compile` and a whole `mago analyze` report it: [`Loaded::Hosted`] becomes the [`Loaded::Analysis`] of the
    /// hosts, from the stubs they declare and the return-type requests the compile recorded. Anything else stays.
    ///
    /// # Errors
    ///
    /// Returns the error of reading the stubs the hosts declare, or of the analysis with them.
    pub(crate) fn taken_over(self, orchestrator: &Orchestrator<'_>) -> Result<Self, Error> {
        let Loaded::Hosted { mut server, requests } = self else {
            return Ok(self);
        };
        orchestrator.add_external_initialization_files(server.database_mut())?;
        let issues = server.analyze_with_hosts(orchestrator.get_analyzer_plugin_registry(), &requests)?.issues;

        Ok(Loaded::Analysis(server, issues))
    }
}

/// What [`compiled`] keeps for a whole `mago analyze`, as [`Loaded`] gives it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keeping {
    /// Nothing, as `mago compile` keeps.
    Nothing,
    /// Every file it loads.
    Files,
    /// Its own analysis.
    Analysis,
    /// Its own analysis, with the return-type requests it records unless the hosts reach every file, and its scans
    /// when they do.
    Hosted,
}

/// The extension hosts a run started, or none when it enables no host.
pub(crate) type Started = Option<Arc<ExternalAnalyzerHandle>>;

/// Compiles the project as `mago analyze` does before it analyzes with the hosts: into the `.sharp` folder as
/// `mago compile --no-extensions` does, and into the hosts' folder as [`compile_for_hosts`] does while it is out of
/// date, from one analysis without the hosts and one lowering. The project's hosts start beside the compile once the
/// hosts' folder holds the files each host's code reaches, and the compile reads what they register before it
/// analyzes. Returns the started hosts, the outcome of the compile into the `.sharp` folder, and what the run reports
/// from when `analysis` is set, as a run that analyzes the files as the compile reads them sets it: the compile's
/// analysis when no host is enabled, and that analysis with the return-type requests it recorded, which the run takes
/// over with the hosts, when one is. With `analysis` unset, the run reports from the files it loaded.
///
/// # Errors
///
/// Returns the error that stops the compile.
pub(crate) fn compile_with_hosts(
    configuration: &Configuration,
    color_choice: ColorChoice,
    analysis: bool,
) -> Result<(Started, Outcome, Loaded), Error> {
    let hosts = HostsCompile::new(configuration)?;
    let mut without_hosts = configuration.clone();
    without_hosts.extension_hosts.clear();
    let keeping = match (analysis, hosts.enabled) {
        (true, false) => Keeping::Analysis,
        (true, true) => Keeping::Hosted,
        (false, _) => Keeping::Files,
    };
    let (outcome, _, loaded, started) =
        compiled(&without_hosts, color_choice, hosts.enabled.then_some(&hosts), keeping)?;

    Ok((started, outcome, loaded))
}

/// Compiles the project with `configuration`, which enables no extension host, into its `.sharp` folder, as [`compile`]
/// does, and into the folder of `hosts`, when given, while that folder is out of date, under the folder's lock as
/// [`compile_for_hosts`] takes it. Each folder is current by its own record, so a `.sharp` folder whose record holds is
/// left as it is, and a hosts' folder out of date beside it, as a cleared cache leaves it, compiles alone as
/// [`compile_for_hosts`] compiles it. Else both compile from the same analysis and lowerings.
///
/// The hosts of `hosts` start beside the compile, once their folder holds a current compiled file of each file
/// [`hosts_reach`] finds a host's code reaches, since a host's file runs as its worker starts, before the compile reads
/// what the host registers, and may construct a class there, as a host that builds its rules or its services does. A
/// stale folder gets those files lowered alone into it first, so the hosts start on them as they are now, and the rest
/// of the project compiles beside the hosts. With `keeping` [`Keeping::Hosted`], what the hosts register decides
/// whether the compile records their return-type requests or keeps its scans. Returns the outcome of the compile into
/// the `.sharp` folder, with the database its issues point into, what `keeping` names, nothing when the compile had
/// nothing to do or when its analysis reports in no file of the project, and the started hosts.
///
/// # Errors
///
/// Returns the error that stops the compile.
fn compiled(
    configuration: &Configuration,
    color_choice: ColorChoice,
    hosts: Option<&HostsCompile>,
    keeping: Keeping,
) -> Result<(Outcome, ReadDatabase, Loaded, Started), Error> {
    let start = || {
        hosts.and_then(|hosts| {
            start_external_analyzer(
                &hosts.extension_hosts,
                configuration.php_version,
                &configuration.analyzer.plugins,
                configuration.analyzer.disable_default_plugins,
                &configuration.environment,
            )
        })
    };
    let workspace = configuration.source.workspace.as_path();
    let root = workspace.canonicalize().map_err(|error| Error::CanonicalizingPath(workspace.to_path_buf(), error))?;
    let compiled_folder = root.join(unit::COMPILED_FOLDER);
    let record_path = compiled_folder.join(INPUTS);
    let mut filesystems = Filesystems::new();
    let orchestrator = whole_workspace(configuration, color_choice, true)?;
    let autoloader = orchestrator.autoloader();
    // A warning or an error the compile doesn't print in its outcome is printed again by the next compile, which a
    // record would skip. The walk below logs its own.
    let warnings = logger::warnings();
    let load = Phase::start("load");
    // The one walk of the project: the record's source files come from it, and the load reads the files it lists.
    let mut listing = orchestrator.list_database(workspace)?;
    let mut sources = source_files(&root, &listing, &mut filesystems, autoloader.as_deref());
    // The hosts' folder, locked, while the project has a PHP# file for the hosts to load, with whether it is out of
    // date.
    let hosts = match hosts.filter(|_| sources.keys().any(|name| name.ends_with(".sharp"))) {
        Some(hosts) => Some((hosts, hosts.lock(&mut listing, &mut sources, &mut filesystems, &orchestrator)?)),
        None => None,
    };
    // A project with no PHP# file gets no `.sharp` folder to lock. The run takes the write lock once it holds the hosts'
    // folder's, and lets it go before it gives that one back as shared, which may wait, so a run that holds the write
    // lock waits on no other lock.
    let writing = sources
        .keys()
        .any(|name| name.ends_with(".sharp"))
        .then(|| mago_database::lock::exclusive(&compiled_folder.join(WRITE_LOCK)))
        .transpose()?;
    let mut last = Record::read(&record_path);
    if let Some(record) = last.as_mut()
        && every_input_is_current(record, configuration, &root, &sources, &mut filesystems)
        && let Some(database) = record.outcome.report_database(&root)?
    {
        drop(writing);
        if let Some((hosts, (lock, held, exclusive, stale))) = hosts {
            let compiled = if stale {
                let phase = Phase::start("hosts");
                let (compiled, _) = silenced(|| {
                    compile_into(configuration, &root, &hosts.folder, &orchestrator, &listing, &sources, filesystems)
                })?;
                phase.end();
                Some(compiled)
            } else {
                None
            };
            hosts.hold(lock, held, exclusive, compiled)?;
        }

        return Ok((std::mem::take(&mut record.outcome), database, Loaded::Nothing, start()));
    }
    let (hosts, held) = match hosts {
        Some((hosts, (lock, held, exclusive, stale))) => (stale.then_some(hosts), Some((hosts, lock, held, exclusive))),
        None => (None, None),
    };
    // A current hosts' folder lets the hosts start while the compile loads the project. A stale one first gets the files
    // the hosts load as their workers start, once the project is loaded.
    let mut started = hosts.is_none().then(start).flatten();
    // The last compile's outputs, which this one keeps unread while their status holds.
    let outputs = last.map(|mut record| std::mem::take(&mut record.outputs)).unwrap_or_default();
    // A record the compile can't delete is in a folder it then fails to write its compiled files to.
    let _ = std::fs::remove_file(&record_path);
    // A project with no PHP# file gets no `.sharp` folder to keep a record in.
    let mut record = sources
        .keys()
        .any(|name| name.ends_with(".sharp"))
        .then(|| {
            Record::start(configuration, &root, &sources, &compiled_folder, filesystems.clone(), autoloader.as_deref())
        })
        .flatten();
    // What the hosts' folder holds: its last compile's outputs, which this one keeps unread while their status holds,
    // what its last compile kept of each file, and the record of what this compile reads and writes there.
    let hosts = hosts.map(|hosts| {
        let record_path = hosts.folder.join(INPUTS);
        let outputs =
            Record::read(&record_path).map(|mut record| std::mem::take(&mut record.outputs)).unwrap_or_default();
        let last = CompiledFiles::decode(
            &std::fs::read(hosts.folder.join(COMPILED_FILES)).unwrap_or_default(),
            BUILD_ID,
            configuration_hash(configuration),
        );
        // A record the compile can't delete is in a folder it then fails to write its compiled files to.
        let _ = std::fs::remove_file(&record_path);
        let record =
            Record::start(configuration, &root, &sources, &hosts.folder, filesystems.clone(), autoloader.as_deref());

        (hosts, outputs, last, record)
    });

    let database = orchestrator.load_listed(workspace, &listing)?.into_static();
    let loaded_files = (keeping == Keeping::Files).then(|| database.clone());
    let (database, included) = with_packages(database);
    load.end();

    let mut errors = Vec::new();
    let library = match standard_library(&root, &database) {
        Ok(library) => library,
        Err(refusal) => {
            let outcome = Outcome { errors, summary: refusal, failed: true, ..Outcome::default() };
            let record = record.filter(|_| logger::warnings() == warnings);
            let (outcome, database) = finish(record, &record_path, outcome, &database, Vec::new())?;
            drop(writing);
            if let Some((hosts, lock, held, exclusive)) = held {
                hosts.hold(lock, held, exclusive, None)?;
            }

            return Ok((
                outcome,
                database,
                loaded_files.map_or(Loaded::Nothing, |files| Loaded::Files(Box::new(files), Scans::default())),
                started.or_else(start),
            ));
        }
    };
    let mut reach = None;
    if let Some((hosts, ..)) = &hosts {
        let phase = Phase::start("hosts");
        reach = Some(silenced(|| write_hosts_reach(configuration, &root, hosts, &sources, &database, &library))?);
        phase.end();
        started = start();
    }

    // The run takes this analysis over with the hosts, so the analysis records what it would ask them, unless they
    // reach every file, which the run then analyzes from scratch from this analysis's scans. A host that fails to load
    // fails the run once the compile wrote both folders without the hosts, so the next run starts from them.
    let (records, unloaded) = match started.as_ref().filter(|_| keeping == Keeping::Hosted) {
        Some(started) => match orchestrator.record_return_type_requests(Arc::clone(started)) {
            Ok(records) => (records, None),
            Err(error) => {
                let error = match &reach {
                    Some(reach) => reach.refusing(&database, error),
                    None => error,
                };

                (false, Some(AnalysisError::from(error)))
            }
        },
        None => (false, None),
    };
    let package_files = package_files(&database, &included);
    let reports = matches!(keeping, Keeping::Analysis | Keeping::Hosted)
        && database.files().any(|file| file.file_type == FileType::Host && !included.contains(&file.id));
    let server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator))
        .keeping_lowering_inputs()
        .with_package_files(package_files);
    // The analysis with the hosts that reads the files this compile loaded, or analyzes them all again, takes this
    // analysis's scan of each.
    let server = if loaded_files.is_some() || (keeping == Keeping::Hosted && !records) {
        server.keeping_scans()
    } else {
        server
    };
    // A run that reports this analysis reports in every file but the packages' `.sharp` files, as its own analysis of
    // the files it loads would. A compile alone needs the node-analysis hooks only in the files it lowers.
    let mut server =
        if reports { server.scoped_to(move |file| !included.contains(&file.id)) } else { server.scoped_to(is_lowered) };
    let issues = server.analyze()?.issues;
    // The requests of this analysis alone: a file the compile analyzes again to lower it makes the same ones.
    let requests = orchestrator.get_analyzer_plugin_registry().take_return_type_requests();
    let hidden = Hidden::new(configuration)?;
    let read_database = server.database().read_only();
    let lean = Lean::new(&root, configuration.threads, &configuration.environment);
    // A law Lean judged is an error of its program, which `PROOFS` keeps, as the record keeps every other refusal. A law
    // checker that couldn't be installed, or a law a generated module refused, depends on what no record holds, so such a
    // compile keeps no record, and the next one checks the law again.
    let mut outcomes_kept = true;
    let stamped = Mutex::new(Vec::new());
    let compiled_files_path = compiled_folder.join(COMPILED_FILES);
    let last = CompiledFiles::decode(
        &std::fs::read(&compiled_files_path).unwrap_or_default(),
        BUILD_ID,
        configuration_hash(configuration),
    );
    let mut hosts_filesystems = filesystems.clone();
    let compiled = |file: &File| match compiled_path_of(&root, &sources, file)? {
        Some(compiled) => read_listed(&compiled),
        None => Ok(None),
    };
    let write = |file: &File, bytes: Vec<u8>| write_compiled(compiled_path_of(&root, &sources, file)?, &bytes);
    let mut intact = |file: &File| match compiled_path_of(&root, &sources, file) {
        Ok(Some(compiled)) => is_unchanged(&compiled, outputs.get(&source_name(&root, &compiled)), &mut filesystems),
        _ => false,
    };
    let mut hosts_files = hosts.as_ref().map(|(hosts, outputs, _, _)| {
        (
            host_compiled(&root, &hosts.folder, &sources),
            host_intact(&root, &hosts.folder, &sources, outputs, &mut hosts_filesystems),
            host_write(&root, &hosts.folder, &sources),
        )
    });
    let hosts_folder = hosts
        .as_ref()
        .zip(hosts_files.as_mut())
        .map(|((_, _, last, _), (compiled, intact, write))| Folder { last, compiled, intact, write });
    // A file whose errors the lowering does not survive is refused in the hosts' folder, whatever panic message its
    // lowering printed.
    let printing = hosts_folder.is_some().then(|| {
        let printing = std::panic::take_hook();
        std::panic::set_hook(Box::new(|panic| tracing::debug!("{panic}")));
        printing
    });
    let compilations = server.compile(
        Folder { last: &last, compiled: &compiled, intact: &mut intact, write: &write },
        hosts_folder,
        recording_stamp(&root, &read_database, &stamped),
        |translations, reach, errored| {
            lean.check(translations, reach, errored).map(|checked| {
                outcomes_kept = checked.outcomes_kept;
                checked.issues
            })
        },
        BUILD_ID,
        &library,
        |issues| hidden.shown(issues, &read_database),
    );
    if let Some(printing) = printing {
        std::panic::set_hook(printing);
    }
    drop(hosts_files);
    let ((compilations, compiled_files), hosts_compiled) = compilations?;
    let write_phase = Phase::start("write");
    let database = server.database();
    let mut stamped = in_path_order(stamped);
    let hosts_stamped = hosts.as_ref().map(|_| stamped.clone());
    stamped.extend(served_stamps(&root, database, autoloader.as_deref())?);

    let mut written = BTreeSet::new();
    let mut accepted = HashSet::new();
    let mut refused = IssueCollection::default();
    let mut refused_files = 0;
    let mut refused_php_files = 0;
    let mut packaged_refusals = Vec::new();
    for (file_id, compilation) in compilations {
        let file = database.get_ref(&file_id)?;
        let name = String::from_utf8_lossy(&file.name);
        let Some(compiled) = compiled_path_of(&root, &sources, file)? else {
            let source = source_path(&root, file);
            errors.push(format!(
                "{name} is a link to {}, outside the project, so it was not compiled. Install the package as a copy instead of a link.",
                source
                    .canonicalize()
                    .map_err(|error| DatabaseError::io(IOAction::ResolvePath, &source, error))?
                    .display()
            ));
            continue;
        };

        match compilation {
            Compilation::Accepted(output) => {
                let Written { hash, .. } = output?;
                if let Some(record) = record.as_mut() {
                    record.output(&compiled, hash);
                }
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Kept => {
                if let Some(record) = record.as_mut()
                    && let Some(output) = outputs.get(&source_name(&root, &compiled))
                {
                    record.unchanged_output(&compiled, *output);
                }
                written.insert(compiled);
                accepted.insert(file_id);
            }
            Compilation::Refused(issues) => {
                refused.extend(issues);
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
    let inputs = listener_inputs(database, |path| stamp_of(&root, database, path))?;
    if !inputs.is_empty() {
        let externs = server.extern_attributes();
        let externs: Vec<&ExternAttributes> = externs.iter().filter(|line| accepted.contains(&line.file_id)).collect();
        let families = server.families();
        let families: Vec<&Family> = families.iter().filter(|family| accepted.contains(&family.file_id)).collect();
        let accepted: Vec<&Listener> =
            listeners.iter().filter(|listener| accepted.contains(&listener.file_id)).collect();
        let effects = server.extern_effects();
        let table = compiled_folder.join(LISTENERS);
        let dispatcher = server.dispatcher_namespace();
        let modules = server.modules();
        let packages = package_folders(&root);
        let bytes = listener_table(&inputs, &accepted, dispatcher, &packages, &externs, &effects, &families, &modules);
        write_if_changed(&table, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&table, unit::source_hash(&bytes));
        }
        written.insert(table);
    }
    if compiled_folder.is_dir() {
        let bytes = compiled_files.encode(BUILD_ID, configuration_hash(configuration))?;
        write_if_changed(&compiled_files_path, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&compiled_files_path, unit::source_hash(&bytes));
        }
        written.insert(compiled_files_path);
        written.insert(compiled_folder.join(PROOFS));
        let codebase = server.codebase();
        let mut types = BTreeSet::new();
        let mut loaded = up_front_files(&root)?;
        for class in codebase.class_likes.values() {
            if class.flags.is_module() || !accepted.contains(&class.span.file_id) {
                continue;
            }
            let file = database.get_ref(&class.span.file_id)?;
            if file.folder_namespace != Some(FolderNamespace::Test) {
                types.insert(file.name.as_ref());
                loaded.insert(class.span.file_id);
            }
        }
        let autoload = compiled_folder.join(AUTOLOAD);
        let bytes = autoload_script(&root)?;
        write_if_changed(&autoload, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&autoload, unit::source_hash(&bytes));
        }
        written.insert(autoload);
        let preload = compiled_folder.join(PRELOAD);
        let bytes = preload_script(&types, &anonymous_class_types(codebase, loaded));
        write_if_changed(&preload, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&preload, unit::source_hash(&bytes));
        }
        written.insert(preload);
        let kept: BTreeSet<PathBuf> = written
            .iter()
            .cloned()
            .chain(record.as_ref().and_then(Record::marker_path))
            .chain([compiled_folder.join(COMPILE_LOCK), compiled_folder.join(WRITE_LOCK)])
            .collect();
        delete_all_but(&compiled_folder, &kept, &compiled_folder.join(PACKAGE_FOLDER))?;
    }

    for folder in package_compiled_folders(&root, database) {
        errors.push(format!(
            "{folder} is a compiled folder inside a package, and the engine would read it instead of the project's .sharp folder. Delete {folder}."
        ));
    }
    errors.extend(packaged_refusals);

    let mut summary = match (accepted.len(), refused_files) {
        (0, 0) => "No PHP# files found to compile.".to_string(),
        (compiled, _) => format!("Compiled {} into .sharp/.", files(compiled)),
    };
    let mut refusals = Vec::new();
    match refused_files {
        0 => {}
        1 => refusals.push("1 PHP# file was refused".to_string()),
        refused => refusals.push(format!("{refused} PHP# files were refused")),
    }
    match refused_php_files {
        0 => {}
        1 => refusals.push("1 plain PHP file was refused".to_string()),
        refused => refusals.push(format!("{refused} plain PHP files were refused")),
    }
    if !refusals.is_empty() {
        summary
            .push_str(&format!(" {}: fix the errors and run vendor/bin/mago compile again.", refusals.join(", and ")));
    }
    let failed = !errors.is_empty() || !refusals.is_empty();
    let outcome = Outcome::new(refused, database, errors, summary, failed);
    let record = record.filter(|_| logger::warnings() == warnings && outcomes_kept);
    let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
    if let (Some((hosts, outputs, _, record)), Some(compiled), Some(stamped)) = (hosts, hosts_compiled, hosts_stamped) {
        let folder = &hosts.folder;
        write_hosts(
            configuration,
            &root,
            folder,
            &sources,
            &server,
            autoloader.as_deref(),
            compiled,
            &outputs,
            stamped,
            &inputs,
            record,
        )?;
    }
    let (outcome, database) = finish(record, &record_path, outcome, database, stamped.into_iter().chain(listed))?;
    write_phase.end();
    drop(writing);
    if let Some((hosts, lock, held, exclusive)) = held {
        hosts.hold(lock, held, exclusive, None)?;
    }
    let loaded = match loaded_files {
        _ if reports && keeping == Keeping::Hosted => Loaded::Hosted { server: Box::new(server), requests },
        _ if reports => Loaded::Analysis(Box::new(server), issues),
        Some(files) => Loaded::Files(Box::new(files), server.take_scans()),
        None => Loaded::Nothing,
    };
    if let Some(error) = unloaded {
        return Err(error.into());
    }

    Ok((outcome, database, loaded, started))
}

/// Keeps `record` at `path`, the record of a compile whose `outcome` it prints again while every input is as it was,
/// with `inputs`, the stamps the compile took. Gives `outcome`, with `database`, which holds the files it names.
///
/// # Errors
///
/// Returns the error of writing the record.
fn finish(
    record: Option<Record>,
    path: &Path,
    outcome: Outcome,
    database: &Database<'_>,
    inputs: impl IntoIterator<Item = Stamp>,
) -> Result<(Outcome, ReadDatabase), Error> {
    if let Some(record) = record {
        record.write(path, outcome.clone(), database, inputs)?;
    }

    Ok((outcome, database.read_only()))
}

/// What a compile printed once it was done, which [`Record`] keeps, so a compile with nothing to do prints it again,
/// a failure included, until an input changes.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Outcome {
    /// The issues of each file the compile refused, which the checker's report prints.
    issues: IssueCollection,
    /// The name of each file an issue points into, whose lines the report prints.
    issue_files: Vec<String>,
    /// Each other error the compile found, as a PHP file someone wrote where a source's PHP file goes.
    errors: Vec<String>,
    /// The line the compile printed last.
    summary: String,
    /// Whether the compile failed, which its exit status says.
    failed: bool,
}

impl Outcome {
    /// The outcome of a compile that refused `issues`, each in a file of `database`, found `errors` beside them, and
    /// printed `summary` last, which `failed` or succeeded.
    fn new(
        issues: IssueCollection,
        database: &impl DatabaseReader,
        errors: Vec<String>,
        summary: String,
        failed: bool,
    ) -> Self {
        let issue_files = issues
            .iter()
            .flat_map(|issue| &issue.annotations)
            .filter_map(|annotation| database.get_ref(&annotation.span.file_id).ok())
            .map(|file| String::from_utf8_lossy(&file.name).into_owned())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();

        Self { issues, issue_files, errors, summary, failed }
    }

    /// The issues of each file the compile refused, and each other error it found.
    pub(crate) fn into_parts(self) -> (IssueCollection, Vec<String>) {
        (self.issues, self.errors)
    }

    /// This outcome with `errors` beside its issues: the errors the extension hosts' analysis reported in the project's
    /// PHP# files that the compile's own analysis did not. The hosts' rules only report, so each file keeps its compiled
    /// file, and the errors fail the run.
    fn with_hosts_errors(mut self, errors: IssueCollection) -> Self {
        let (reported, them) = match errors.len() {
            0 => return self,
            1 => ("1 error".to_string(), "it"),
            count => (format!("{count} errors"), "them"),
        };
        self.issues.extend(errors);
        self.summary.push_str(&format!(
            " The extension hosts reported {reported} in PHP# files: fix {them} and run vendor/bin/mago compile again."
        ));
        self.failed = true;

        self
    }

    /// The files the issues point into, for the report of a compile with nothing to do: each file in the project of
    /// `root`, as the compile read it, which every input being as it was keeps, and each of the prelude's. None when
    /// [`read_listed`] finds one of the project's gone since the record was checked, which makes the record stale.
    ///
    /// # Errors
    ///
    /// Returns the error of [`read_listed`].
    fn report_database(&self, root: &Path) -> Result<Option<ReadDatabase>, Error> {
        let mut database = Database::new(DatabaseConfiguration::new(root, vec![], vec![], vec![], vec![]));
        let mut prelude = None;
        for name in &self.issue_files {
            let path = root.join(name);
            if let Some(contents) = read_listed(&path)? {
                let name = name.as_bytes().to_vec().into();
                database.add(File::new(name, FileType::Host, Some(path), contents.into()));
                continue;
            }
            let prelude = prelude.get_or_insert_with(|| {
                Prelude::decode_database(PRELUDE_BYTES).expect("Failed to decode embedded prelude")
            });
            let Ok(file) = prelude.get_by_name(name.as_bytes()) else {
                return Ok(None);
            };
            database.add(File::new(file.name.clone(), file.file_type, file.path.clone(), file.contents.clone()));
        }

        Ok(Some(database.read_only()))
    }

    /// Prints the outcome: the checker's report of each issue, with `configuration`'s editor links and in
    /// `color_choice`, its lines read from `database`, then each other error, then the summary. Gives the compile's
    /// exit status.
    ///
    /// # Errors
    ///
    /// Returns the error of printing the report.
    fn print(
        self,
        database: ReadDatabase,
        configuration: &Configuration,
        color_choice: ColorChoice,
    ) -> Result<CommandOutcome, Error> {
        if !self.issues.is_empty() {
            let reporter = Reporter::new(
                database,
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
                    tool_version: VERSION,
                },
            );
            reporter.report(self.issues, None)?;
        }
        for error in &self.errors {
            tracing::error!("{error}");
        }
        if self.failed {
            tracing::error!("{}", self.summary);

            return Ok(ExitCode::FAILURE.into());
        }
        tracing::info!("{}", self.summary);

        Ok(ExitCode::SUCCESS.into())
    }
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

/// The status and hash of each file a compile wrote or kept, by name, as [`Record`] keeps them.
type Outputs = BTreeMap<String, (Status, [u8; 16])>;

/// What a compile read and wrote, as [`INPUTS`] holds it: the header the compile ran under, each source file as the
/// compile found it before it read it, with the hash of the bytes it read, the stamp of each other input, and the
/// status of each file it wrote or kept right after, with the hash of its bytes.
#[derive(Serialize, Deserialize)]
struct Record {
    /// The build ID of the checker that wrote it.
    build: [u8; 16],
    /// The project's canonical root.
    root: PathBuf,
    /// The hash of the configuration, as [`configuration_hash`] gives it.
    configuration: [u8; 16],
    /// What the compile printed once it was done, a failure's issues and errors included.
    outcome: Outcome,
    /// The modification time of the record's own temp file, which the compile created when it started, in nanoseconds
    /// since the epoch, as the filesystem's clock gave it.
    marker: i64,
    /// Each file [`source_files`] found other than an output, by name.
    files: BTreeMap<String, (Found, [u8; 16])>,
    /// Each other input, as [`recorded`] holds it.
    inputs: Vec<Stamp>,
    /// Each file the compile wrote or kept, by name.
    outputs: Outputs,
    /// The record's own temp file, whose modification time is the marker, which the compile keeps until it is done.
    #[serde(skip)]
    marker_file: Option<tempfile::NamedTempFile>,
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
    /// reads it, each absent one below it, Composer's `installed.json`, the autoload maps of `autoloader`, the baseline,
    /// and the `.sharp` folder of each folder that holds a package's PHP# file, absent unless the compile refuses it. `filesystems` tells which
    /// filesystems record every write, for the outputs' statuses. Gives none when the temp file can't be created or an
    /// input can't be stamped, and the compile then keeps no record.
    fn start(
        configuration: &Configuration,
        root: &Path,
        sources: &BTreeMap<String, Found>,
        folder: &Path,
        mut filesystems: Filesystems,
        autoloader: Option<&Autoloader>,
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
        if let Some(autoloader) = autoloader {
            let composer = autoloader.vendor().join("composer");
            for map in MAPS {
                inputs.push(stamp(source_name(root, &composer.join(map)).as_bytes())?);
            }
        }
        if let Some(baseline) = &configuration.analyzer.baseline {
            inputs.push(stamp(baseline.as_os_str().as_encoded_bytes())?);
        }

        Some(Self {
            build: BUILD_ID,
            root: root.to_path_buf(),
            configuration: configuration_hash(configuration),
            outcome: Outcome::default(),
            marker,
            files: sources.iter().map(|(name, found)| (name.clone(), (found.clone(), [0; 16]))).collect(),
            inputs,
            outputs: BTreeMap::new(),
            marker_file: Some(marker_file),
            lost: false,
            filesystems,
        })
    }

    /// The path of the record's own temp file, which the compile keeps in its folder until it is done.
    fn marker_path(&self) -> Option<PathBuf> {
        self.marker_file.as_ref().map(|file| file.path().to_path_buf())
    }

    /// Keeps the file at `path`, which the compile wrote or kept with bytes whose [`unit::source_hash`] is `hash`, as an
    /// output with its status now.
    fn output(&mut self, path: &Path, hash: [u8; 16]) {
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let name = source_name(&self.root, path);
                let status = Status::of(path, &metadata, &mut self.filesystems);
                self.files.remove(&name);
                self.outputs.insert(name, (status, hash));
            }
            Err(_) => self.lost = true,
        }
    }

    /// Keeps the file at `path`, which the compile kept unread, as an output with `output`, the status and hash the last
    /// compile recorded for it, which [`is_unchanged`] found it still has.
    fn unchanged_output(&mut self, path: &Path, output: (Status, [u8; 16])) {
        let name = source_name(&self.root, path);
        self.files.remove(&name);
        self.outputs.insert(name, output);
    }

    /// Writes the record to `path` once the compile is done with `outcome`: hashes each source file's bytes as
    /// `database` read them, or as the file holds them when the database didn't read it, and adds `inputs`, the stamps
    /// the compile took. A compile whose output changed since it wrote or kept it, as a file someone swapped in or a
    /// compile that ran at once changes it, keeps no record.
    ///
    /// # Errors
    ///
    /// Returns the error of reading a file or of writing the record.
    fn write(
        mut self,
        path: &Path,
        outcome: Outcome,
        database: &impl DatabaseReader,
        inputs: impl IntoIterator<Item = Stamp>,
    ) -> Result<(), Error> {
        let changed = self.outputs.iter().any(|(name, (status, _))| {
            let output = self.root.join(name);
            std::fs::metadata(&output)
                .map_or(true, |metadata| Status::of(&output, &metadata, &mut self.filesystems) != *status)
        });
        if self.lost || changed {
            return Ok(());
        }
        let mut files = BTreeMap::new();
        for (name, (found, _)) in std::mem::take(&mut self.files) {
            let hash = match database.get_by_name(name.as_bytes()) {
                Ok(file) => unit::source_hash(&file.contents),
                Err(_) => match read_listed(&self.root.join(&name))? {
                    Some(bytes) => unit::source_hash(&bytes),
                    None => continue,
                },
            };
            files.insert(name, (found, hash));
        }
        self.inputs.extend(inputs);
        self.inputs.retain(|(path, ..)| !files.contains_key(path));
        self.inputs.sort_unstable();
        self.inputs.dedup();
        self.files = files;
        self.outcome = outcome;
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
/// `.sharp` folder of a project whose first compile refused every file and warned doesn't, so no compile leaves an
/// empty folder.
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

    record.build == BUILD_ID
        && record.root == root
        && record.configuration == configuration_hash(configuration)
        && found.len() == record.files.len()
        && found.iter().zip(&record.files).all(|((name, Found(_, real)), (recorded, (Found(_, recorded_real), _)))| {
            *name == recorded && real == recorded_real
        })
        && record.outputs.iter().all(|(name, output)| {
            let path = root.join(name);
            is_unchanged(&path, Some(output), filesystems) || hashes_as(&path, &output.1)
        })
        && record.inputs.par_iter().all(|(path, size, hash)| match unit::stamp(root, path.as_bytes()) {
            Ok(Some(input)) => (input.size, input.hash) == (*size, *hash),
            // On a disk that ignores case, the loader reads a file whose name differs only in case.
            Ok(None) => (*size, *hash) == (0, [0; 16]) && !root.join(path).exists(),
            Err(_) => false,
        })
        && found.iter().zip(record.files.values()).all(|((name, Found(status, _)), (Found(recorded, _), hash))| {
            (status == recorded && status.is_older_than(record.marker)) || hashes_as(&root.join(name), hash)
        })
}

/// Whether the file at `path`, an output a compile recorded as `output`, is the one it wrote: its status is the one
/// recorded, on a filesystem whose status change time records every write. `filesystems` tells which do.
fn is_unchanged(path: &Path, output: Option<&(Status, [u8; 16])>, filesystems: &mut Filesystems) -> bool {
    output.is_some_and(|(status, _)| {
        status.sees_writes()
            && std::fs::metadata(path).is_ok_and(|metadata| Status::of(path, &metadata, filesystems) == *status)
    })
}

/// An input as [`Record`] holds it: its path, size and hash, both zero when no file is there.
type Stamp = (String, u64, [u8; 16]);

/// The `stamp` a compile of the project at `root` takes, which stamps each input it is given as [`stamp_of`] does from
/// `database`, and keeps the stamp in `stamped`, from whichever thread stamps it.
fn recording_stamp<'a>(
    root: &'a Path,
    database: &'a ReadDatabase,
    stamped: &'a Mutex<Vec<Stamp>>,
) -> impl Fn(&[u8]) -> Result<Option<unit::Input>, DatabaseError> + Sync + 'a {
    move |path| {
        let input = stamp_of(root, database, path)?;
        stamped.lock().unwrap_or_else(PoisonError::into_inner).push(recorded(path, input.as_ref()));

        Ok(input)
    }
}

/// The stamp of the input at `path` of the project at `root`: from the bytes the loader read into `database`, as
/// [`unit::loaded_stamp`] gives it, so a file the analysis read is not read again, and from disk, as [`unit::stamp`]
/// reads it, for every other input.
fn stamp_of(root: &Path, database: &impl DatabaseReader, path: &[u8]) -> Result<Option<unit::Input>, DatabaseError> {
    let loaded = database.get_id(path).and_then(|id| database.get_ref(&id).ok());
    match loaded.and_then(unit::loaded_stamp) {
        Some(input) => Ok(Some(input)),
        None => unit::stamp(root, path),
    }
}

/// The stamps [`recording_stamp`] kept, in path order, which no order of the threads that took them changes.
fn in_path_order(stamped: Mutex<Vec<Stamp>>) -> Vec<Stamp> {
    let mut stamped = stamped.into_inner().unwrap_or_else(PoisonError::into_inner);
    stamped.sort_unstable();

    stamped
}

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

/// The hash of the settings of `configuration` that change what a compile writes: the PHP version, the sources, and the
/// parser's and the analyzer's settings, from their debug form, which holds every field, the ones serde skips included,
/// as the deprecated `perform-heuristic-checks`. The extension hosts, the threads, the stack size, the version checks,
/// the linter's, the formatter's and the guard's settings, the editor URL, the configuration file and the command-line
/// flags change nothing a compile writes, since every compile lowers from the analysis without the hosts, so runs that
/// differ only there share one compile.
fn configuration_hash(configuration: &Configuration) -> [u8; 16] {
    let Configuration { php_version, source, parser, analyzer, .. } = configuration;

    unit::source_hash(format!("{php_version:?}{source:?}{parser:?}{analyzer:?}").as_bytes())
}

/// Each file the loader reads, as the walk of `listing` found it, by the name the database gives it, as [`Found`]
/// holds it: its status, from the metadata the walk read before any file was read, and its real path's name when that
/// differs. Each file `autoloader`'s maps serve is left out, which the record stamps when the analysis reads it.
/// `filesystems` tells which filesystems record every write.
fn source_files(
    root: &Path,
    listing: &Listing,
    filesystems: &mut Filesystems,
    autoloader: Option<&Autoloader>,
) -> BTreeMap<String, Found> {
    let mut files = BTreeMap::new();
    for listed in listing.files() {
        if autoloader.is_some_and(|autoloader| autoloader.serves(&listed.path)) {
            continue;
        }
        let name = source_name(root, &listed.path);
        let real = Some(source_name(root, &listed.real)).filter(|real| *real != name);
        files.insert(name, Found(Status::of(&listed.path, &listed.metadata, filesystems), real));
    }

    files
}

/// The name the database gives the file at `path` in the project at `root`, as [`File::logical_name`] gives it.
fn source_name(root: &Path, path: &Path) -> String {
    String::from_utf8_lossy(&File::logical_name(root, path)).into_owned()
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
    let source = source_path(root, &file);
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
/// folder of each and of each folder above it, which a PHP# file or a namespace folder joining or leaving changes, each
/// as `stamp` gives it. A project with no PHP# file has none.
fn listener_inputs(
    database: &impl DatabaseReader,
    stamp: impl Fn(&[u8]) -> Result<Option<unit::Input>, DatabaseError> + Sync,
) -> Result<Vec<unit::Input>, Error> {
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

    let paths: Vec<Vec<u8>> = paths.into_iter().collect();
    // Stamped at once, and the first file in order that can't be read is the one named, as one after another.
    let stamped: Vec<Result<Option<unit::Input>, DatabaseError>> = paths.par_iter().map(|path| stamp(path)).collect();
    let stamped: Vec<Option<unit::Input>> = stamped.into_iter().collect::<Result<_, _>>()?;

    Ok(stamped.into_iter().flatten().collect())
}

/// The PHP source of the listener table. Its `inputs` are the files whose change makes the table out of date, each as
/// its workspace-relative path, size, modification time and hash. Its `listeners` are one row per listener, in the
/// order the engine runs them, of its class, its method, the events its `on` clause lists, and whether it takes the
/// event. A full name holds no `'` and never ends in `\`, so each one is a single-quoted string as written. Its
/// `dispatcher` is the namespace, in lowercase, of the app's modules whose binding of the dispatcher serves the whole
/// app, or `null`. Its `packages` are the folders Composer installs the project's packages in, as
/// [`package_folders`] gives them, in which the engine finds a plain PHP method's own package, spec section 29. Its
/// `attributes` are, for each plain PHP class, method and function an `extern` line gives attributes,
/// the files whose lines do, which the engine compiles when reflection reads its attributes, spec section 29. A class
/// is keyed by its full name, a method as `class::method`, and a function as `function()`, each in lowercase. Its
/// `effects` are, keyed the same way, the effects the first `extern` line of each plain PHP class and method declares,
/// the standard library's included, by their dotted full names, or `null` for a line that says `uses any`, which
/// `Method.createDelegate` holds a plain PHP method to, spec section 25. Its `tags` are, for each family, spec section
/// 10, under its full name, the key its `[Tag]` names and each variant's class under the variant's name, which a
/// family's `parse` and `withTag` read. Its `modules` are the files of the base modules of each namespace, under the
/// namespace in lowercase, and those of its environment modules, under the namespace, a space and the environment's
/// name, which the engine reads for a class of the namespace or of a namespace below it wherever the class's file is,
/// spec section 32.
#[allow(clippy::too_many_arguments)]
fn listener_table(
    inputs: &[unit::Input],
    listeners: &[&Listener],
    dispatcher: Option<Word>,
    packages: &[String],
    externs: &[&ExternAttributes],
    effects: &[ExternEffects],
    families: &[&Family],
    modules: &[RecordedModule],
) -> Vec<u8> {
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
        Some(namespace) => {
            quoted(&mut table, namespace.as_bytes());
            table.extend_from_slice(b",\n");
        }
        None => table.extend_from_slice(b"null,\n"),
    }
    table.extend_from_slice(b"    'packages' => [\n");
    for folder in packages {
        table.extend_from_slice(b"        ");
        quoted(&mut table, folder.as_bytes());
        table.extend_from_slice(b",\n");
    }
    table.extend_from_slice(b"    ],\n");
    // `externs` holds each declaration's lines together.
    let mut declarations: Vec<(Word, Vec<Word>)> = Vec::new();
    for line in externs {
        match declarations.last_mut() {
            Some((declaration, files)) if *declaration == line.declaration => files.push(line.file),
            _ => declarations.push((line.declaration, vec![line.file])),
        }
    }
    table.extend_from_slice(b"    'attributes' => [\n");
    for (declaration, files) in declarations {
        table.extend_from_slice(b"        ");
        quoted(&mut table, declaration.as_bytes());
        table.extend_from_slice(b" => [");
        for (index, file) in files.iter().enumerate() {
            if index > 0 {
                table.extend_from_slice(b", ");
            }
            quoted(&mut table, file.as_bytes());
        }
        table.extend_from_slice(b"],\n");
    }
    table.extend_from_slice(b"    ],\n    'effects' => [\n");
    for declared in effects {
        table.extend_from_slice(b"        ");
        quoted(&mut table, declared.declaration.as_bytes());
        let Some(listed) = &declared.effects else {
            table.extend_from_slice(b" => null,\n");
            continue;
        };
        table.extend_from_slice(b" => [");
        for (index, effect) in listed.iter().enumerate() {
            if index > 0 {
                table.extend_from_slice(b", ");
            }
            quoted(&mut table, effect.as_bytes());
        }
        table.extend_from_slice(b"],\n");
    }
    table.extend_from_slice(b"    ],\n    'tags' => [\n");
    for family in families {
        table.extend_from_slice(b"        ");
        quoted(&mut table, family.class.as_bytes());
        table.extend_from_slice(b" => [");
        quoted(&mut table, family.key.as_bytes());
        table.extend_from_slice(b", [");
        for (index, (name, variant)) in family.variants.iter().enumerate() {
            if index > 0 {
                table.extend_from_slice(b", ");
            }
            quoted(&mut table, name.as_bytes());
            table.extend_from_slice(b" => ");
            quoted(&mut table, variant.as_bytes());
        }
        table.extend_from_slice(b"]],\n");
    }
    // `modules` holds each key's modules together.
    let mut keyed: Vec<(String, Vec<Word>)> = Vec::new();
    for module in modules {
        let key = match module.environment {
            Some(environment) => format!("{} {environment}", module.namespace),
            None => module.namespace.to_string(),
        };
        match keyed.last_mut() {
            Some((last, files)) if *last == key => files.push(module.file),
            _ => keyed.push((key, vec![module.file])),
        }
    }
    table.extend_from_slice(b"    ],\n    'modules' => [\n");
    for (key, files) in keyed {
        table.extend_from_slice(b"        ");
        quoted(&mut table, key.as_bytes());
        table.extend_from_slice(b" => [");
        for (index, file) in files.iter().enumerate() {
            if index > 0 {
                table.extend_from_slice(b", ");
            }
            quoted(&mut table, file.as_bytes());
        }
        table.extend_from_slice(b"],\n");
    }
    table.extend_from_slice(b"    ],\n];\n");

    table
}

/// The folder Composer installs each package of the project at `root` in, as its nearest `composer.json` and
/// `composer.lock` list them under its `config.vendor-dir`, from `root` and ending in `/`, as `vendor/acme/format/`.
fn package_folders(root: &Path) -> Vec<String> {
    nearest_manifest(root, &mut Manifests::default()).map_or_else(Vec::new, |manifest| {
        manifest.package_folders().map(|folder| format!("{}/", source_name(root, &folder))).collect()
    })
}

/// The PHP source of `autoload.php`, spec section 27, which requires the Composer autoloader of the vendor folder of
/// `root`, the one `composer.json`'s `config.vendor-dir` names, else `vendor`. A vendor folder inside the project is
/// named from the `.sharp` folder, so the project runs wherever it is moved.
///
/// # Errors
///
/// Returns the error of reading `composer.json`.
fn autoload_script(root: &Path) -> Result<Vec<u8>, Error> {
    let vendor = vendor_folder(root).map_err(|error| OrchestratorError::General(error.to_string()))?;
    let autoloader = vendor.join("autoload.php");
    let mut script = b"<?php\n\n// mago compile writes this file, the project's Composer autoloader. It rewrites it on every compile.\n\nrequire_once ".to_vec();
    match autoloader.strip_prefix(root) {
        Ok(inside) => {
            script.extend_from_slice(b"__DIR__ . ");
            quoted(&mut script, &[b"/../", inside.as_os_str().as_encoded_bytes()].concat());
        }
        Err(_) => quoted(&mut script, autoloader.as_os_str().as_encoded_bytes()),
    }
    script.extend_from_slice(b";\n");

    Ok(script)
}

/// The PHP source of the preload script, spec section 27, for `types`, the workspace-relative name of each source whose
/// type the compile accepted, a test's left out. It requires the project's Composer autoloader through `autoload.php`,
/// then each source, so a type whose parent another file declares, plain PHP or PHP#, gets its parent from the
/// autoloader, as in a request. Then it loads each of `named`, a type an anonymous class names, through the function
/// its kind's check takes.
fn preload_script(types: &BTreeSet<&[u8]>, named: &BTreeSet<(&str, &[u8])>) -> Vec<u8> {
    let mut script = b"<?php\n\n// mago compile writes this file. It rewrites it on every compile.\n\nrequire_once __DIR__ . '/autoload.php';\n".to_vec();
    for name in types {
        script.extend_from_slice(b"require_once __DIR__ . ");
        quoted(&mut script, &[b"/../", *name].concat());
        script.extend_from_slice(b";\n");
    }
    for (check, name) in named {
        script.extend_from_slice(check.as_bytes());
        script.push(b'(');
        quoted(&mut script, name);
        script.extend_from_slice(b");\n");
    }

    script
}

/// The files Composer's autoloader in `root` loads up front, `autoload_files.php`'s, which the preload loads with it.
///
/// # Errors
///
/// Returns the error of reading `composer.json` or Composer's autoload maps.
fn up_front_files(root: &Path) -> Result<HashSet<FileId>, Error> {
    let unreadable = |error: AutoloadError| OrchestratorError::General(error.to_string());
    let Some(autoload) = Autoload::read(&vendor_folder(root).map_err(unreadable)?).map_err(unreadable)? else {
        return Ok(HashSet::new());
    };

    Ok(autoload.files().iter().map(|path| FileId::new(&File::logical_name(root, path))).collect())
}

/// Each type outside PHP's own an anonymous class names as its parent, an interface or a trait, with the function that
/// loads it, for each anonymous class in a file the preload loads: one of `loaded`, or the file of a type a loaded
/// file's types name so. PHP declares an anonymous class only when its code runs, so nothing else loads what it names
/// before opcache links the preloaded classes, and opcache would warn that it can't preload it.
fn anonymous_class_types(codebase: &CodebaseMetadata, mut loaded: HashSet<FileId>) -> BTreeSet<(&'static str, &[u8])> {
    let mut declared: HashMap<FileId, Vec<&ClassLikeMetadata>> = HashMap::new();
    for class in codebase.class_likes.values() {
        declared.entry(class.span.file_id).or_default().push(class);
    }

    let mut named = BTreeSet::new();
    let mut unread: Vec<FileId> = loaded.iter().copied().collect();
    while let Some(file) = unread.pop() {
        for class in declared.get(&file).into_iter().flatten() {
            let names =
                class.direct_parent_class.iter().chain(&class.direct_parent_interfaces).chain(&class.used_traits);
            let used = names.filter_map(|name| codebase.get_class_like(*name));
            for used in used.filter(|used| !used.flags.is_built_in()) {
                if loaded.insert(used.span.file_id) {
                    unread.push(used.span.file_id);
                }
                if class.name_span.is_none() {
                    let check = match used.kind {
                        SymbolKind::Trait => "trait_exists",
                        SymbolKind::Interface => "interface_exists",
                        SymbolKind::Class | SymbolKind::Enum => "class_exists",
                    };
                    named.insert((check, used.original_name.as_bytes()));
                }
            }
        }
    }

    named
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
    use crate::config::test_environment;
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
        let configuration = Configuration::load(
            CliOverrides {
                workspace: Some(root.clone()),
                config: Some(root.join("mago.toml")),
                no_version_check: true,
                ..Default::default()
            },
            test_environment(),
        )
        .unwrap();

        (directory, root, configuration)
    }

    /// Whether the PHP SDK's Composer dependencies are installed, which `MAGO_REQUIRE_PHP_SDK_TESTS` requires.
    fn php_sdk() -> bool {
        let available = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/autoload.php").is_file();
        assert!(
            available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(),
            "vendor dependencies are required for the extension host tests"
        );
        available
    }

    /// An extension host whose return-type provider types each call of `App\Box::value()` as `string`.
    const BOX_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\MethodReturnTypeProvider;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class Boxes implements Plugin, MethodReturnTypeProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('boxes', 'Boxes', 'Types the value of a box.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerMethodReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [MethodTarget::allMethods('App\\Box')];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        return $context->invocation->name === 'value' ? Type::string() : null;
    }
}

(new Worker(new Extension('boxes', 'Boxes', '0.0.0', analyzerPlugins: [new Boxes()])))->run();
"#;

    /// Each file under `folder`, its record of inputs included, by its path, with its modification time and its bytes.
    fn files_as_written(folder: &Path) -> BTreeMap<PathBuf, (std::time::SystemTime, Vec<u8>)> {
        let mut files = BTreeMap::new();
        let mut folders = vec![folder.to_path_buf()];
        while let Some(next) = folders.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                    files.insert(path.clone(), (modified, std::fs::read(&path).unwrap()));
                }
            }
        }
        files
    }

    /// Each file under `folder` that `run` wrote, added or deleted.
    fn written_under(folder: &Path, run: impl FnOnce()) -> BTreeSet<PathBuf> {
        let before = files_as_written(folder);
        run();
        let after = files_as_written(folder);

        before.keys().chain(after.keys()).filter(|path| before.get(*path) != after.get(*path)).cloned().collect()
    }

    /// The project of [`project`] with `App\Box`, whose `value()` the extension host [`BOX_HOST`] types, a plain PHP
    /// function that reads it, and the host enabled, with its configuration.
    fn boxed_project() -> (tempfile::TempDir, PathBuf, Configuration) {
        let (directory, root, _) = project();
        write(
            &root,
            "app/Box.php",
            "<?php\n\nnamespace App;\n\nfinal class Box\n{\n    public function value()\n    {\n        return 1;\n    }\n}\n",
        );
        write(
            &root,
            "app/Reader.php",
            "<?php\n\nnamespace App;\n\nfunction read_value(Box $box): string\n{\n    return $box->value();\n}\n",
        );
        write(&root, "tools/host.php", BOX_HOST);
        write(
            &root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.boxes]\nfile = \"tools/host.php\"\n",
        );
        let configuration = Configuration::load(
            CliOverrides {
                workspace: Some(root.clone()),
                config: Some(root.join("mago.toml")),
                no_version_check: true,
                ..Default::default()
            },
            test_environment(),
        )
        .unwrap();

        (directory, root, configuration)
    }

    /// Spec section 27: the `.sharp` folder holds the one compile of a tree, written from the analysis without the
    /// extension hosts whichever command writes it, so with a host that types a call, `mago compile` and the compile a
    /// whole `mago analyze` runs each leave what the other wrote, in either order.
    #[test]
    fn a_compile_and_a_whole_analyze_s_compile_of_a_tree_the_other_left_current_write_nothing() {
        if !php_sdk() {
            return;
        }
        let (_directory, root, configuration) = boxed_project();
        let folder = root.join(unit::COMPILED_FOLDER);
        let mago_compile = || {
            compile(&configuration, ColorChoice::Never).unwrap();
        };
        let whole_analyze = || {
            compile_with_hosts(&configuration, ColorChoice::Never, true).unwrap();
        };

        mago_compile();
        assert_eq!(written_under(&folder, whole_analyze), BTreeSet::new(), "the whole analyze after the compile");
        assert_eq!(written_under(&folder, mago_compile), BTreeSet::new(), "the compile after the whole analyze");
    }

    /// Spec section 27: the hosts' folder is a cache of its own, current by its own record, so a run whose hosts'
    /// folder lost its record, as a cleared cache leaves it, compiles the hosts' folder alone and writes nothing under
    /// a `.sharp` folder whose record holds.
    #[test]
    fn a_compile_whose_hosts_folder_alone_is_stale_writes_nothing_under_sharp() {
        if !php_sdk() {
            return;
        }
        let (_directory, root, configuration) = boxed_project();
        let hosts = HostsCompile::new(&configuration).unwrap().folder;
        let mago_compile = || {
            compile(&configuration, ColorChoice::Never).unwrap();
        };
        mago_compile();
        std::fs::remove_file(hosts.join(INPUTS)).unwrap();

        assert_eq!(written_under(&root.join(unit::COMPILED_FOLDER), mago_compile), BTreeSet::new());
        assert!(Record::read(&hosts.join(INPUTS)).is_some(), "the hosts' folder is compiled again");
    }

    /// A `vendor/` file a PHP# file calls is no input of its compiled file, which `composer.lock` stands for, so the
    /// record of the hosts' compile's source files is what sees an edit of it.
    #[test]
    fn the_hosts_compile_runs_again_after_an_edit_of_a_vendor_file() {
        let (_directory, root, configuration) = project();
        let folder = tempfile::tempdir().unwrap();
        let compile = || HostsCompile::at(&configuration, &root, folder.path()).compile_whole(false).unwrap();
        assert!(matches!(compile(), HostsChange::Recompiled(_)), "a fresh folder is written");
        // A compile that runs writes its record anew, though every compiled file it writes is the same.
        let recorded = || std::fs::metadata(folder.path().join(INPUTS)).unwrap().modified().unwrap();
        let written = recorded();
        assert!(matches!(compile(), HostsChange::Unchanged), "nothing changed");
        assert_eq!(recorded(), written, "the compile had nothing to do");

        write(&root, "vendor/acme/rate/src/Rate.php", &rate("string"));
        let paced = std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap();

        assert!(matches!(compile(), HostsChange::Recompiled(_)), "the vendor edit compiles again");
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

        let Err(error) = HostsCompile::at(&configuration, &root, folder.path()).compile_whole(false) else {
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

    /// Each file of a hosts' folder but its record of inputs, by its path in the folder.
    fn folder_files(folder: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut folders = vec![folder.to_path_buf()];
        while let Some(below) = folders.pop() {
            for entry in std::fs::read_dir(below).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if path != folder.join(INPUTS) {
                    files.insert(path.strip_prefix(folder).unwrap().to_path_buf(), std::fs::read(&path).unwrap());
                }
            }
        }

        files
    }

    /// A watch compiles each edit for its hosts from its last compile, and each round writes the folder a fresh compile
    /// of the tree writes, its record of the compiled files byte for byte included, keeps a record that stands for the
    /// tree, and writes no compiled file whose bytes the edit leaves.
    #[test]
    fn each_watch_round_of_the_hosts_compile_writes_what_a_fresh_compile_of_the_tree_writes() {
        let (_directory, root, configuration) = project();
        let folder = tempfile::tempdir().unwrap();
        let mut hosts = HostsCompile::at(&configuration, &root, folder.path());
        assert!(matches!(hosts.compile_whole(true).unwrap(), HostsChange::Recompiled(_)), "a fresh folder is written");
        let written = |name: &str| std::fs::metadata(folder.path().join(name)).unwrap().modified().unwrap();
        let round = |hosts: &mut HostsCompile, changed: &str| {
            let orchestrator = whole_workspace(&configuration, ColorChoice::Never, false).unwrap();
            let (database, _) = project_database(&orchestrator, &root).unwrap();
            let change = hosts.recompile(&database, &[FileId::new(changed.as_bytes())]).unwrap();

            let fresh = tempfile::tempdir().unwrap();
            HostsCompile::at(&configuration, &root, fresh.path()).compile_whole(false).unwrap();
            assert_eq!(folder_files(folder.path()), folder_files(fresh.path()), "after the change of {changed}");
            let mut filesystems = Filesystems::new();
            let sources = source_files(&root, &listing(&configuration), &mut filesystems, None);
            assert!(
                every_compiled_file_is_current(&configuration, &root, folder.path(), &sources, &mut filesystems),
                "the record stands for the tree after the change of {changed}"
            );

            change
        };

        let paced = written("app/Paced.sharpc");
        write(&root, "app/Order.sharp", "namespace App;\n\npublic class Order\n{\n    public int total() => 2;\n}\n");
        let HostsChange::Recompiled(Recompiled { files, .. }) = round(&mut hosts, "app/Order.sharp") else {
            panic!("the body edit compiles again");
        };
        assert_eq!(files, HashSet::from([FileId::new(b"app/Order.sharp")]));
        assert_eq!(written("app/Paced.sharpc"), paced, "the compiled file of Paced is not written again");

        write(&root, "vendor/acme/rate/src/Rate.php", &rate("string"));
        let HostsChange::Recompiled(Recompiled { files, .. }) = round(&mut hosts, "vendor/acme/rate/src/Rate.php")
        else {
            panic!("the vendor edit compiles again");
        };
        assert!(files.contains(&FileId::new(b"app/Paced.sharp")), "Paced calls the edited Rate");

        write(
            &root,
            "app/Invoice.sharp",
            "namespace App;\n\npublic class Invoice\n{\n    public int total() => 3;\n}\n",
        );
        assert!(matches!(round(&mut hosts, "app/Invoice.sharp"), HostsChange::Recompiled(_)), "a new file compiles");
        assert!(folder.path().join("app/Invoice.sharpc").exists());

        std::fs::remove_file(root.join("app/Invoice.sharp")).unwrap();
        assert!(matches!(round(&mut hosts, "app/Invoice.sharp"), HostsChange::Recompiled(_)), "a deleted file is gone");
        assert!(!folder.path().join("app/Invoice.sharpc").exists());
    }

    /// A watch that starts on a folder another run compiled, as each watch after the first does, compiles each round
    /// only what the files the watch names reach, and analyzes, lowers and writes nothing else, so an edit the watch
    /// doesn't name keeps the compiled file the last compile wrote.
    #[test]
    fn a_watch_round_of_the_hosts_compile_compiles_only_what_the_files_it_names_reach() {
        let (_directory, root, configuration) = project();
        let folder = tempfile::tempdir().unwrap();
        HostsCompile::at(&configuration, &root, folder.path()).compile_whole(false).unwrap();
        let mut hosts = HostsCompile::at(&configuration, &root, folder.path());
        let (database, _) =
            project_database(&whole_workspace(&configuration, ColorChoice::Never, false).unwrap(), &root).unwrap();
        assert!(matches!(hosts.recompile(&database, &[]).unwrap(), HostsChange::Unchanged), "the folder is current");
        let paced = std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap();

        write(&root, "app/Order.sharp", "namespace App;\n\npublic class Order\n{\n    public int total() => 2;\n}\n");
        write(&root, "vendor/acme/rate/src/Rate.php", &rate("string"));
        let (database, _) =
            project_database(&whole_workspace(&configuration, ColorChoice::Never, false).unwrap(), &root).unwrap();
        let HostsChange::Recompiled(Recompiled { files, .. }) =
            hosts.recompile(&database, &[FileId::new(b"app/Order.sharp")]).unwrap()
        else {
            panic!("the body edit compiles again");
        };

        assert_eq!(files, HashSet::from([FileId::new(b"app/Order.sharp")]));
        assert_eq!(std::fs::read(folder.path().join("app/Paced.sharpc")).unwrap(), paced);
    }

    /// Every file a compile of the project of `configuration` walks to, as [`compiled`] lists them.
    fn listing(configuration: &Configuration) -> Listing {
        whole_workspace(configuration, ColorChoice::Never, false)
            .unwrap()
            .list_database(&configuration.source.workspace)
            .unwrap()
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
        let sources = source_files(&root, &listing(&configuration), &mut forced, None);
        let mut record = Record::start(&configuration, &root, &sources, folder, forced.clone(), None).unwrap();
        record.output(&output, unit::source_hash(b"compiled"));
        record.write(&folder.join(INPUTS), Outcome::default(), &ReadDatabase::empty(), []).unwrap();
        let mut current = || {
            let sources = source_files(&root, &listing(&configuration), &mut forced, None);
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

    /// Linux's FAT and exFAT keep no status change time, and the stat of a FUSE, 9p, NFS, CIFS or SMB file can be one
    /// the client cached while the file's bytes are new, so none of them records every write. ext4 does.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_names_fat_exfat_and_the_filesystems_whose_clients_cache_a_stat_as_recording_no_write() {
        let fat = 0x4d44;
        let exfat = 0x2011_bab0;
        let fuse = 0x6573_5546;
        let nine_p = 0x0102_1997;
        let nfs = 0x6969;
        let cifs = 0xff53_4d42;
        let smb2 = 0xfe53_4d42;
        let smb = 0x517b;

        for magic in [fat, exfat, fuse, nine_p, nfs, cifs, smb2, smb] {
            assert!(!records_writes(magic), "{magic:#x} records every write");
        }
        assert!(records_writes(0xef53), "ext4 records no write");
        let directory = tempfile::tempdir().unwrap();
        assert!(change_time_records_writes(directory.path()), "the temp folder's filesystem records every write");
    }

    /// macOS's FAT and exFAT keep no status change time, and the stat of an NFS, SMB, WebDAV or AFP file can be one the
    /// client cached while the file's bytes are new, so none of them records every write. APFS does.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_names_fat_exfat_and_the_filesystems_whose_clients_cache_a_stat_as_recording_no_write() {
        for name in [b"msdos".as_slice(), b"exfat", b"nfs", b"smbfs", b"webdav", b"afpfs"] {
            assert!(!records_writes(name), "{} records every write", String::from_utf8_lossy(name));
        }
        assert!(records_writes(b"apfs"), "APFS records no write");
        let directory = tempfile::tempdir().unwrap();
        assert!(change_time_records_writes(directory.path()), "the temp folder's filesystem records every write");
    }

    /// Each setting that changes what a compile writes enters the record's header, the ones serde skips included, so a
    /// change to any of them makes the next compile run, and a change to any other setting keeps the record current.
    #[test]
    fn only_a_setting_that_changes_what_a_compile_writes_changes_the_configuration_hash() {
        let base = Configuration::from_workspace(PathBuf::from("/project"));
        type Change = fn(&mut Configuration);
        let written: [(&str, Change); 5] = [
            ("php-version", |changed| changed.php_version = PHPVersion::PHP80),
            ("source", |changed| changed.source.excludes.push("legacy".to_owned())),
            ("parser", |changed| changed.parser.enable_short_tags ^= true),
            ("analyzer", |changed| changed.analyzer.find_unused_definitions ^= true),
            ("analyzer.perform-heuristic-checks", |changed| changed.analyzer.perform_heuristic_checks = Some(false)),
        ];
        let unwritten: [(&str, Change); 13] = [
            ("extension-hosts", |changed| {
                changed.extension_hosts.insert("acme".to_owned(), ExtensionHostConfiguration::default());
            }),
            ("version", |changed| changed.version = Some("0.6".to_owned())),
            ("threads", |changed| changed.threads += 1),
            ("stack-size", |changed| changed.stack_size += 1),
            ("allow-unsupported-php-version", |changed| changed.allow_unsupported_php_version ^= true),
            ("no-version-check", |changed| changed.no_version_check ^= true),
            ("version-drift-fail-level", |changed| changed.version_drift_fail_level = VersionDriftFailLevel::Patch),
            ("linter", |changed| changed.linter.excludes.push("legacy".to_owned())),
            ("formatter", |changed| changed.formatter.settings.print_width += 1),
            ("guard", |changed| changed.guard.excludes.push("legacy".to_owned())),
            ("editor-url", |changed| changed.editor_url = Some("phpstorm://open?file=%file%".to_owned())),
            ("config-file", |changed| changed.config_file = Some(PathBuf::from("/project/mago.toml"))),
            ("cli-overrides", |changed| changed.cli_overrides.threads = Some(4)),
        ];

        let hash_after = |change: Change| {
            let mut changed = base.clone();
            change(&mut changed);
            configuration_hash(&changed)
        };
        for (field, change) in written {
            assert_ne!(hash_after(change), configuration_hash(&base), "{field}");
        }
        for (field, change) in unwritten {
            assert_eq!(hash_after(change), configuration_hash(&base), "{field}");
        }
    }

    /// The report of a compile with nothing to do reads each file its issues point into, so a file gone since the
    /// record's check, deleted or moved, makes the record stale and the compile runs in full, while a file still there
    /// is read.
    #[test]
    fn a_file_the_reprinted_issues_point_into_that_is_gone_makes_the_record_stale() {
        let (_directory, root, _) = project();
        let outcome = |names: &[&str]| Outcome {
            issue_files: names.iter().map(|name| (*name).to_owned()).collect(),
            ..Outcome::default()
        };

        assert!(outcome(&["app/Order.sharp", "app/Gone.sharp"]).report_database(&root).unwrap().is_none());
        let read = outcome(&["app/Order.sharp"]).report_database(&root).unwrap().expect("app/Order.sharp is there");
        assert!(read.get_by_name(b"app/Order.sharp").is_ok());
    }
}
