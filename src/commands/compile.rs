//! `mago compile`: checks every PHP# file, the ones in `vendor/` too, and writes the `.sharpc` file the engine runs
//! for each accepted one.
//!
//! Every compiled file lives in one `.sharp` folder at the workspace root, at the real path of its source relative to
//! the root, with `.sharp` replaced by `.sharpc`. The folder mirrors the accepted sources exactly: a refused, removed
//! or renamed source loses its compiled file. It writes nothing outside that folder.
//!
//! It writes `preload.php` in the `.sharp` folder too, the script `opcache.preload` names to preload every type it
//! accepted outside `autoload-dev` folders, spec section 27.
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
use std::sync::Mutex;
use std::sync::PoisonError;

use clap::ColorChoice;
use clap::Parser;
use rayon::iter::IntoParallelRefIterator;
use rayon::iter::ParallelIterator;
use serde::Deserialize;
use serde::Serialize;

use mago_analyzer::code::IssueCode;
use mago_build_id::BUILD_ID;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
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
use mago_database::loader::read_listed;
use mago_database::membership::WorkspaceMatcher;
use mago_orchestrator::Autoloader;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::OrchestratorError;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_orchestrator::service::incremental_analysis::compile::Compiled;
use mago_orchestrator::service::incremental_analysis::compile::CompiledFiles;
use mago_orchestrator::service::incremental_analysis::compile::DispatcherModule;
use mago_orchestrator::service::incremental_analysis::compile::ExternAttributes;
use mago_orchestrator::service::incremental_analysis::compile::ExternEffects;
use mago_orchestrator::service::incremental_analysis::compile::Family;
use mago_orchestrator::service::incremental_analysis::compile::Folder;
use mago_orchestrator::service::incremental_analysis::compile::Listener;
use mago_prelude::Prelude;
use mago_reporting::CompiledIgnoreSet;
use mago_reporting::IssueCollection;
use mago_reporting::ReportingFormat;
use mago_reporting::ReportingTarget;
use mago_reporting::color::ColorChoice as ReportingColorChoice;
use mago_reporting::reporter::Reporter;
use mago_reporting::reporter::ReporterConfig;
use mago_server::Server;
use mago_sharp_bridge::unit;
use mago_sharp_lean::Lean;
use mago_sharp_lean::PACKAGE_FOLDER;
use mago_sharp_lean::PROOFS;
use mago_syntax::dialect::Dialect;
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
    let compiled = if hosts.enabled { silenced(|| hosts.compile_whole(false))? } else { HostsChange::Unchanged };

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
        let folder = mago_sharp_lean::cache_root()
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
        server, autoloader, filesystems, library, compiled_files: last_compiled, outputs, stamps, ..
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
        let (mut compiled, mut intact) = (host_compiled(root, folder), host_intact(root, folder, outputs, filesystems));
        server.compile(
            Folder { last: last_compiled, compiled: &mut compiled, intact: &mut intact },
            None,
            |path| {
                let input = remembered(path)?;
                stamped.lock().unwrap_or_else(PoisonError::into_inner).push(recorded(path, input.as_ref()));

                Ok(input)
            },
            |_, _, _| Ok(Default::default()),
            library,
            |_| IssueCollection::default(),
        )
    };
    std::panic::set_hook(printing);
    let (recompiled, next_outputs, compiled_files) = write_hosts(
        configuration,
        root,
        folder,
        server,
        autoloader.as_deref(),
        compilations?.0,
        outputs,
        in_path_order(stamped),
        remembered,
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

/// Writes into `folder`, for the extension hosts of the project at `root` with `configuration`, the compiled files of
/// `compiled`, which a compile made from `server`'s analysis, and the folder's other files, and keeps the record of
/// what the compile read and wrote in `folder`'s [`INPUTS`], which `record` starts, with `stamped`, the stamps the
/// compile took, those of the files `autoloader` serves, and those `stamp` gives the listeners' inputs. A compiled file
/// the compile kept keeps the status and hash `outputs` holds for it. Returns, when a file in `folder` changed, the
/// files whose compiled file changed, then the status and hash of each file the compile wrote or kept, and what the
/// next compile keeps files by.
#[allow(clippy::too_many_arguments)]
fn write_hosts(
    configuration: &Configuration,
    root: &Path,
    folder: &Path,
    server: &Server,
    autoloader: Option<&Autoloader>,
    (compilations, compiled_files): Compiled,
    outputs: &Outputs,
    mut stamped: Vec<Stamp>,
    stamp: impl Fn(&[u8]) -> Result<Option<unit::Input>, DatabaseError> + Sync,
    mut record: Option<Record>,
) -> Result<(Option<Recompiled>, Outputs, CompiledFiles), Error> {
    let record_path = folder.join(INPUTS);
    let compiled_files_path = folder.join(COMPILED_FILES);
    let database = server.database();
    stamped.extend(served_stamps(root, database, autoloader)?);

    // Whether a file a host's worker loads changed, so a watch knows to start the workers again.
    let mut changed = false;
    let mut keep = |path: &Path, bytes: &[u8], wrote: bool| {
        changed |= wrote;
        if let Some(record) = record.as_mut() {
            record.output(path, bytes);
        }
    };
    let mut written = BTreeSet::new();
    let mut refused = IssueCollection::default();
    let mut accepted = HashSet::new();
    let mut recompiled = HashSet::new();
    let bytes = compiled_files.encode(configuration_hash(configuration))?;
    keep(&compiled_files_path, &bytes, write_if_changed(&compiled_files_path, &bytes)?);
    written.insert(compiled_files_path);
    let mut writes = Vec::new();
    let mut kept = Vec::new();
    for (file_id, compilation) in compilations {
        let file = database.get_ref(&file_id)?;
        let Some(compiled) = host_compiled_path(root, folder, &source_path(root, file))? else {
            continue;
        };
        match compilation {
            Compilation::Accepted(bytes) => {
                writes.push((file_id, compiled, bytes));
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
    // Each file is read and written on its own, so they are written at once and kept in the record in compile order.
    let wrote: Vec<Result<bool, DatabaseError>> =
        writes.par_iter().map(|(_, compiled, bytes)| write_if_changed(compiled, bytes)).collect();
    for ((file_id, compiled, bytes), wrote) in writes.into_iter().zip(wrote) {
        let wrote = wrote?;
        if wrote {
            recompiled.insert(file_id);
        }
        keep(&compiled, &bytes, wrote);
        written.insert(compiled);
    }
    let classes = server
        .codebase()
        .class_likes
        .values()
        .filter(|class| recompiled.contains(&class.span.file_id))
        .map(|class| ascii_lowercase_word(class.original_name.as_bytes()))
        .collect();

    let inputs = listener_inputs(database, stamp)?;
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
        let dispatcher = server.dispatcher_module();
        let bytes = listener_table(&inputs, &accepted, dispatcher.as_ref(), &externs, &effects, &families);
        keep(&table, &bytes, write_if_changed(&table, &bytes)?);
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
    keep(&path, &report, write_if_changed(&path, &report)?);
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

/// The compiled file of a file in `folder`, the hosts' folder of the project at `root`, or none when no file is there.
fn host_compiled<'a>(
    root: &'a Path,
    folder: &'a Path,
) -> impl FnMut(&File) -> Result<Option<Vec<u8>>, DatabaseError> + 'a {
    move |file: &File| match host_compiled_path(root, folder, &source_path(root, file))? {
        Some(compiled) => read_listed(&compiled),
        None => Ok(None),
    }
}

/// Whether the compiled file of a file in `folder`, the hosts' folder of the project at `root`, is the file the last
/// compile wrote, by the status `outputs` holds for it and `filesystems`.
fn host_intact<'a>(
    root: &'a Path,
    folder: &'a Path,
    outputs: &'a Outputs,
    filesystems: &'a mut Filesystems,
) -> impl FnMut(&File) -> bool + 'a {
    move |file: &File| match host_compiled_path(root, folder, &source_path(root, file)) {
        Ok(Some(compiled)) => is_unchanged(&compiled, outputs.get(&source_name(root, &compiled)), filesystems),
        _ => false,
    }
}

/// Where the hosts' compile of the project at `root` into `folder` puts the compiled file of `source`: where
/// `unit::compiled_path` puts it in the `.sharp` folder, in `folder` instead, or none when `source` is a link outside
/// `root`.
fn host_compiled_path(root: &Path, folder: &Path, source: &Path) -> Result<Option<PathBuf>, DatabaseError> {
    let compiled_folder = root.join(unit::COMPILED_FOLDER);

    Ok(unit::compiled_path(root, source)?
        .and_then(|compiled| compiled.strip_prefix(&compiled_folder).ok().map(|relative| folder.join(relative))))
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

/// Whether the compile lowers `file`, a PHP# file of the project, which is the scope of its analysis. The compile
/// refuses a PHP# file for the errors reported in it, and a node-analysis hook reports only in the file it inspects, so
/// the hooks run only in these files, as they run only in the named files of `mago analyze <paths>`.
fn is_lowered(file: &File) -> bool {
    file.file_type == FileType::Host && Dialect::of(file).is_sharp()
}

/// Checks every PHP# file the analyzer loads, writes the compiled file of each accepted one, and deletes every other
/// file in the `.sharp` folder, as [`CompileCommand::execute`] says.
fn compile(configuration: &Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
    let (outcome, database, _) = compiled(configuration, color_choice, None, Keeping::Nothing)?;

    outcome.print(database, configuration, color_choice)
}

/// What a whole `mago analyze` reports from, of what its compile loaded and analyzed.
pub(crate) enum Loaded {
    /// Nothing, when the compile had nothing to do: the run loads the files itself.
    Nothing,
    /// Every file the compile loaded, as `mago analyze` loads them with no extension host started, which the analysis
    /// with the hosts reads instead of loading them again.
    Files(Database<'static>),
    /// The compile's own analysis, which a project with no extension host enabled reports instead of analyzing again,
    /// and its issues. Its database holds each package's `.sharp` file as a file of the project, which the compile
    /// checks too, and it reports in every file but those, whose refusals the compile's outcome holds.
    Analysis(Box<Server>, IssueCollection),
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
}

/// Compiles the project as `mago analyze` does before it analyzes with the hosts: into the `.sharp` folder as
/// `mago compile --no-extensions` does, and into the hosts' folder as [`compile_for_hosts`] does while it is out of
/// date, from one analysis without the hosts and one lowering. Returns the project's hosts, as [`compile_for_hosts`]
/// gives them, the outcome of the compile into the `.sharp` folder, and what the run reports from: the compile's
/// analysis when no host is enabled and `analysis` is set, as a run that analyzes the files as the compile reads them
/// sets it, and the files it loaded otherwise.
///
/// # Errors
///
/// Returns the error that stops the compile.
pub(crate) fn compile_with_hosts(
    configuration: &Configuration,
    color_choice: ColorChoice,
    analysis: bool,
) -> Result<(BTreeMap<String, ExtensionHostConfiguration>, Outcome, Loaded), Error> {
    let hosts = HostsCompile::new(configuration)?;
    let mut without_hosts = configuration.clone();
    without_hosts.extension_hosts.clear();
    let keeping = if analysis && !hosts.enabled { Keeping::Analysis } else { Keeping::Files };
    let (outcome, _, loaded) = compiled(&without_hosts, color_choice, hosts.enabled.then_some(&hosts), keeping)?;

    Ok((hosts.extension_hosts, outcome, loaded))
}

/// Compiles the project with `configuration` into its `.sharp` folder, as [`compile`] does, and into the folder of
/// `hosts`, when given, while that folder is out of date, from the same analysis and lowerings, under the folder's
/// lock as [`compile_for_hosts`] takes it. Returns the outcome of the compile into the `.sharp` folder, with the
/// database its issues point into, and what `keeping` names, nothing when the compile had nothing to do or when its
/// analysis reports in no file of the project.
///
/// # Errors
///
/// Returns the error that stops the compile.
fn compiled(
    configuration: &Configuration,
    color_choice: ColorChoice,
    hosts: Option<&HostsCompile>,
    keeping: Keeping,
) -> Result<(Outcome, ReadDatabase, Loaded), Error> {
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
    // A host names none of what it reads, so only a compile without hosts can know it has nothing to do.
    if !configuration.extension_hosts.values().any(|host| host.enabled)
        && hosts.as_ref().is_none_or(|(_, (_, _, _, stale))| !stale)
        && let Some(record) = last.as_mut()
        && every_input_is_current(record, configuration, &root, &sources, &mut filesystems)
        && let Some(database) = record.outcome.report_database(&root)?
    {
        drop(writing);
        if let Some((hosts, (lock, held, exclusive, _))) = hosts {
            hosts.hold(lock, held, exclusive, None)?;
        }

        return Ok((std::mem::take(&mut record.outcome), database, Loaded::Nothing));
    }
    let (hosts, held) = match hosts {
        Some((hosts, (lock, held, exclusive, stale))) => (stale.then_some(hosts), Some((hosts, lock, held, exclusive))),
        None => (None, None),
    };
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
            configuration_hash(configuration),
        );
        // A record the compile can't delete is in a folder it then fails to write its compiled files to.
        let _ = std::fs::remove_file(&record_path);
        let record =
            Record::start(configuration, &root, &sources, &hosts.folder, filesystems.clone(), autoloader.as_deref());

        (hosts, outputs, last, record)
    });

    if let Some(external_analyzer) = start_external_analyzer(
        &configuration.extension_hosts,
        configuration.php_version,
        &configuration.analyzer.plugins,
        configuration.analyzer.disable_default_plugins,
    ) {
        orchestrator.set_external_analyzer_handle(external_analyzer);
    }

    let database = orchestrator.load_listed(workspace, &listing)?.into_static();
    let loaded = if keeping == Keeping::Files { Loaded::Files(database.clone()) } else { Loaded::Nothing };
    let (database, included) = with_packages(database);

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

            return Ok((outcome, database, loaded));
        }
    };
    let package_files: Vec<FileId> = database
        .files()
        .filter(|file| included.contains(&file.id) && package_of(&file.name).is_some())
        .map(|file| file.id)
        .collect();
    let reports = keeping == Keeping::Analysis
        && database.files().any(|file| file.file_type == FileType::Host && !included.contains(&file.id));
    let server = Server::new(database.into_static(), decode_prelude, server_settings(&orchestrator))
        .keeping_lowering_inputs()
        .with_package_files(package_files);
    // A run that reports this analysis reports in every file but the packages' `.sharp` files, as its own analysis of
    // the files it loads would. A compile alone needs the node-analysis hooks only in the files it lowers.
    let mut server =
        if reports { server.scoped_to(move |file| !included.contains(&file.id)) } else { server.scoped_to(is_lowered) };
    let issues = server.analyze()?.issues;
    // The compile hides what `mago analyze` hides: a host rule's error an `ignore` entry or the baseline targets.
    let ignored =
        CompiledIgnoreSet::compile(&configuration.analyzer.ignore, configuration.source.glob.to_database_settings());
    let baseline = match configuration.analyzer.baseline.as_deref().filter(|path| path.exists()) {
        Some(path) => Some(unserialize_baseline(path)?.0),
        None => None,
    };
    let read_database = server.database().read_only();
    let lean = Lean::new(&root, configuration.threads);
    let stamped = Mutex::new(Vec::new());
    let compiled_files_path = compiled_folder.join(COMPILED_FILES);
    let last = CompiledFiles::decode(
        &std::fs::read(&compiled_files_path).unwrap_or_default(),
        configuration_hash(configuration),
    );
    let mut hosts_filesystems = filesystems.clone();
    let mut compiled = |file: &File| match unit::compiled_path(&root, &source_path(&root, file))? {
        Some(compiled) => read_listed(&compiled),
        None => Ok(None),
    };
    let mut intact = |file: &File| match unit::compiled_path(&root, &source_path(&root, file)) {
        Ok(Some(compiled)) => is_unchanged(&compiled, outputs.get(&source_name(&root, &compiled)), &mut filesystems),
        _ => false,
    };
    let mut hosts_files = hosts.as_ref().map(|(hosts, outputs, _, _)| {
        (host_compiled(&root, &hosts.folder), host_intact(&root, &hosts.folder, outputs, &mut hosts_filesystems))
    });
    let hosts_folder = hosts.as_ref().zip(hosts_files.as_mut()).map(|((_, _, last, _), (compiled, intact))| Folder {
        last,
        compiled,
        intact,
    });
    // A file whose errors the lowering does not survive is refused in the hosts' folder, whatever panic message its
    // lowering printed.
    let printing = hosts_folder.is_some().then(|| {
        let printing = std::panic::take_hook();
        std::panic::set_hook(Box::new(|panic| tracing::debug!("{panic}")));
        printing
    });
    let compilations = server.compile(
        Folder { last: &last, compiled: &mut compiled, intact: &mut intact },
        hosts_folder,
        recording_stamp(&root, &read_database, &stamped),
        |translations, reach, errored| lean.check(translations, reach, errored),
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
    );
    if let Some(printing) = printing {
        std::panic::set_hook(printing);
    }
    drop(hosts_files);
    let ((compilations, compiled_files), hosts_compiled) = compilations?;
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
    let mut writes = Vec::new();
    for (file_id, compilation) in compilations {
        let file = database.get_ref(&file_id)?;
        let name = String::from_utf8_lossy(&file.name);
        let source = source_path(&root, file);
        let Some(compiled) = unit::compiled_path(&root, &source)? else {
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
            Compilation::Accepted(bytes) => {
                writes.push((compiled, bytes));
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
    // Each file is read and written on its own, so they are written at once and kept in the record in compile order.
    let wrote: Vec<Result<bool, DatabaseError>> =
        writes.par_iter().map(|(compiled, bytes)| write_if_changed(compiled, bytes)).collect();
    wrote.into_iter().collect::<Result<Vec<bool>, _>>()?;
    for (compiled, bytes) in writes {
        if let Some(record) = record.as_mut() {
            record.output(&compiled, &bytes);
        }
        written.insert(compiled);
    }

    let listeners = server.listeners();
    let inputs = listener_inputs(database, |path| unit::stamp(&root, path))?;
    if !inputs.is_empty() {
        let externs = server.extern_attributes();
        let externs: Vec<&ExternAttributes> = externs.iter().filter(|line| accepted.contains(&line.file_id)).collect();
        let families = server.families();
        let families: Vec<&Family> = families.iter().filter(|family| accepted.contains(&family.file_id)).collect();
        let accepted: Vec<&Listener> =
            listeners.iter().filter(|listener| accepted.contains(&listener.file_id)).collect();
        let effects = server.extern_effects();
        let table = compiled_folder.join(LISTENERS);
        let dispatcher = server.dispatcher_module();
        let bytes = listener_table(&inputs, &accepted, dispatcher.as_ref(), &externs, &effects, &families);
        write_if_changed(&table, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&table, &bytes);
        }
        written.insert(table);
    }
    if compiled_folder.is_dir() {
        let bytes = compiled_files.encode(configuration_hash(configuration))?;
        write_if_changed(&compiled_files_path, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&compiled_files_path, &bytes);
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
        let preload = compiled_folder.join(PRELOAD);
        let bytes = preload_script(&types, &anonymous_class_types(codebase, loaded));
        write_if_changed(&preload, &bytes)?;
        if let Some(record) = record.as_mut() {
            record.output(&preload, &bytes);
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
    let record = record.filter(|_| logger::warnings() == warnings && !outcome.refused_by_law_checker());
    let listed = inputs.iter().map(|input| recorded(&input.path, Some(input)));
    if let (Some((hosts, outputs, _, record)), Some(compiled), Some(stamped)) = (hosts, hosts_compiled, hosts_stamped) {
        let stamp = |path: &[u8]| unit::stamp(&root, path);
        let folder = &hosts.folder;
        write_hosts(
            configuration,
            &root,
            folder,
            &server,
            autoloader.as_deref(),
            compiled,
            &outputs,
            stamped,
            stamp,
            record,
        )?;
    }
    let (outcome, database) = finish(record, &record_path, outcome, database, stamped.into_iter().chain(listed))?;
    drop(writing);
    if let Some((hosts, lock, held, exclusive)) = held {
        hosts.hold(lock, held, exclusive, None)?;
    }
    let loaded = if reports { Loaded::Analysis(Box::new(server), issues) } else { loaded };

    Ok((outcome, database, loaded))
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

    /// Whether the law checker refused a file, as one it could not install or a proof it stopped at its deadline. What
    /// it did depends on what no record holds, the checker's install and the machine's load, so a compile with such a
    /// refusal keeps no record, and the next one runs the checker again.
    fn refused_by_law_checker(&self) -> bool {
        let law_checker = [IssueCode::LawCheckerUnavailable.as_str(), IssueCode::LawUnproven.as_str()];

        self.issues.iter().any(|issue| issue.code.as_deref().is_some_and(|code| law_checker.contains(&code)))
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
            build: BUILD_ID.to_be_bytes(),
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

    record.build == BUILD_ID.to_be_bytes()
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

/// The hash of the settings of `configuration` that change what a compile writes: the PHP version, the sources, the
/// extension hosts, and the parser's and the analyzer's settings, from their debug form, which holds every field, the
/// ones serde skips included, as the deprecated `perform-heuristic-checks`. The threads, the stack size, the version
/// checks, the linter's, the formatter's and the guard's settings, the editor URL, the configuration file and the
/// command-line flags change nothing a compile writes, so runs that differ only there share one compile.
fn configuration_hash(configuration: &Configuration) -> [u8; 16] {
    let Configuration { php_version, source, extension_hosts, parser, analyzer, .. } = configuration;

    unit::source_hash(format!("{php_version:?}{source:?}{extension_hosts:?}{parser:?}{analyzer:?}").as_bytes())
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
/// `dispatcher` is the app's module whose binding of the dispatcher serves the whole app, as its path and namespace,
/// or `null`. Its `attributes` are, for each plain PHP class, method and function an `extern` line gives attributes,
/// the files whose lines do, which the engine compiles when reflection reads its attributes, spec section 29. A class
/// is keyed by its full name, a method as `class::method`, and a function as `function()`, each in lowercase. Its
/// `effects` are, keyed the same way, the effects the first `extern` line of each plain PHP class and method declares,
/// the standard library's included, by their dotted full names, which `Method.createDelegate` holds a plain PHP method
/// to, spec section 25. Its `tags` are, for each family, spec section 10, under its full name, the key its
/// `[Tag]` names and each variant's class under the variant's name, which a family's `parse` and `withTag` read.
fn listener_table(
    inputs: &[unit::Input],
    listeners: &[&Listener],
    dispatcher: Option<&DispatcherModule>,
    externs: &[&ExternAttributes],
    effects: &[ExternEffects],
    families: &[&Family],
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
        Some(module) => {
            table.push(b'[');
            quoted(&mut table, module.file.as_bytes());
            table.extend_from_slice(b", ");
            quoted(&mut table, module.namespace.as_bytes());
            table.extend_from_slice(b"],\n");
        }
        None => table.extend_from_slice(b"null,\n"),
    }
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
        table.extend_from_slice(b" => [");
        for (index, effect) in declared.effects.iter().enumerate() {
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
    table.extend_from_slice(b"    ],\n];\n");

    table
}

/// The PHP source of the preload script, spec section 27, for `types`, the workspace-relative name of each source whose
/// type the compile accepted, a test's left out. It requires the workspace's Composer autoloader, then each source, so
/// a type whose parent another file declares, plain PHP or PHP#, gets its parent from the autoloader, as in a request.
/// Then it loads each of `named`, a type an anonymous class names, through the function its kind's check takes.
fn preload_script(types: &BTreeSet<&[u8]>, named: &BTreeSet<(&str, &[u8])>) -> Vec<u8> {
    let mut script = b"<?php\n\n// mago compile writes this file. It rewrites it on every compile.\n\nrequire_once __DIR__ . '/../vendor/autoload.php';\n".to_vec();
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
        record.output(&output, b"compiled");
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

    /// Each setting that changes what a compile writes enters the record's header, the ones serde skips included, so a
    /// change to any of them makes the next compile run, and a change to any other setting keeps the record current.
    #[test]
    fn only_a_setting_that_changes_what_a_compile_writes_changes_the_configuration_hash() {
        let base = Configuration::from_workspace(PathBuf::from("/project"));
        type Change = fn(&mut Configuration);
        let written: [(&str, Change); 6] = [
            ("php-version", |changed| changed.php_version = PHPVersion::PHP80),
            ("source", |changed| changed.source.excludes.push("legacy".to_owned())),
            ("extension-hosts", |changed| {
                changed.extension_hosts.insert("acme".to_owned(), ExtensionHostConfiguration::default());
            }),
            ("parser", |changed| changed.parser.enable_short_tags ^= true),
            ("analyzer", |changed| changed.analyzer.find_unused_definitions ^= true),
            ("analyzer.perform-heuristic-checks", |changed| changed.analyzer.perform_heuristic_checks = Some(false)),
        ];
        let unwritten: [(&str, Change); 12] = [
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
