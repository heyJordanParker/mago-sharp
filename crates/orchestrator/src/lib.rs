//! Orchestrator for managing and coordinating Mago's analysis tools.
//!
//! The orchestrator crate provides a high-level interface for running various static analysis
//! tasks on PHP codebases. It coordinates between the database, parser, analyzer, linter,
//! formatter, and architectural guard to provide a unified workflow.
//!
//! # Architecture
//!
//! The orchestrator follows a service-oriented architecture where each tool (linter, analyzer,
//! formatter, guard) is encapsulated in its own service. The [`Orchestrator`] struct acts as
//! a factory and coordinator, managing:
//!
//! - **Database**: File system scanning and caching via [`mago_database::Database`]
//! - **Codebase**: Metadata and symbol references via [`mago_codex`]
//! - **Services**: Tool-specific services that operate on the database and codebase
//!
//! # Services
//!
//! The orchestrator provides four main services:
//!
//! - [`LintService`]: Runs linting rules on PHP code
//! - [`IncrementalAnalysisService`](service::incremental_analysis::IncrementalAnalysisService): Performs static analysis
//! - [`GuardService`]: Enforces architectural rules
//! - [`FormatService`]: Formats PHP code
//!
//! # Workflow
//!
//! A typical workflow involves:
//!
//! 1. Create an [`Orchestrator`] with an [`OrchestratorConfiguration`]
//! 2. Load the database using [`Orchestrator::load_database`]
//! 3. Obtain the desired service (e.g., [`Orchestrator::get_lint_service`])
//! 4. Run the service to get results

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use foldhash::HashSet;
use mago_allocator::LocalArena;
use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::plugin::create_registry_with_plugins;
use mago_codex::metadata::CodebaseMetadata;
use mago_composer::autoload::Autoload;
use mago_composer::autoload::vendor_folder;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::ReadDatabase;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::exclusion::Exclusion;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_database::loader::DatabaseLoader;
use mago_database::loader::read_listed;
use mago_database::membership::WorkspaceMatcher;
use mago_sharp_bridge::unit::COMPILED_FOLDER;

use crate::service::format::FileFormatStatus;
use crate::service::format::FormatService;
use crate::service::guard::GuardService;
use crate::service::lint::LintService;

pub use config::OrchestratorConfiguration;
pub use error::OrchestratorError;

pub mod config;
pub mod error;
pub mod progress;
pub mod service;

/// The main orchestrator for running operations on PHP code.
///
/// The [`Orchestrator`] is the central coordinator that provides factory methods for creating
/// various services (linting, analysis, formatting, guarding) and manages the shared configuration
/// and database loading.
///
/// # Responsibilities
///
/// - **Configuration Management**: Stores and provides access to the configuration for all services
/// - **Database Loading**: Handles file system scanning and database initialization
/// - **Service Creation**: Acts as a factory for creating tool-specific services
/// - **Path Management**: Manages source paths and exclusion patterns
#[derive(Debug)]
pub struct Orchestrator<'cfg> {
    /// Configuration for all operations.
    pub config: OrchestratorConfiguration<'cfg>,
    /// Plugin registry for the analyzer, lazily initialized.
    plugin_registry: OnceLock<Arc<PluginRegistry>>,
    /// Original source paths moved aside by [`set_source_paths`](Self::set_source_paths).
    ///
    /// These are included as context during analysis but never converted to excludes,
    /// since they represent the user's own code, not external dependencies.
    context_paths: Vec<String>,
    /// The autoload maps [`Self::autoload_vendor`] read, which serve an analysis the vendor folder's plain PHP files.
    autoloader: Option<Arc<Autoloader>>,
}

/// Composer's autoload maps of the vendor folder `includes` names.
///
/// They serve an analysis each plain PHP file of the folder once it reaches a class the file declares, as PHP's
/// autoloader reads the file once the code first names the class. The database reads the folder's `.sharp` files and
/// Composer's own folder whole.
#[derive(Debug)]
pub struct Autoloader {
    autoload: Autoload,
    vendor: PathBuf,
    workspace: PathBuf,
    /// Classifies each file as the database would with the vendor folder read whole.
    matcher: WorkspaceMatcher,
    /// The folders of the vendor folder whose plain PHP files only the maps serve: every folder no other `paths`,
    /// `includes` or `patches` entry and no Composer folder lies in, whose own files the database would read.
    served: Vec<PathBuf>,
}

impl Autoloader {
    /// The vendor folder the maps serve.
    #[must_use]
    pub fn vendor(&self) -> &Path {
        &self.vendor
    }

    /// Whether the file at `path` is one only the maps serve, a plain PHP file of a served folder, which the database
    /// leaves out.
    #[must_use]
    pub fn serves(&self, path: &Path) -> bool {
        path.extension().is_none_or(|extension| extension != "sharp")
            && self.served.iter().any(|folder| path.starts_with(folder))
    }

    /// The plain PHP file the maps place `class` in, as the database would hold it, or `None` when no map places it in
    /// a file the database would hold among the vendor folder's.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] when the file can't be read.
    pub(crate) fn load(&self, class: &[u8]) -> Result<Option<File>, DatabaseError> {
        self.autoload.locate(class).map_or(Ok(None), |path| self.read(&path))
    }

    /// The files Composer loads on every request, `autoload_files.php`'s, among those the database would hold among the
    /// vendor folder's.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] when a file can't be read.
    pub(crate) fn files(&self) -> Result<Vec<File>, DatabaseError> {
        self.autoload.files().iter().filter_map(|path| self.read(path).transpose()).collect()
    }

    /// The file at `path` as the database would hold it among the vendor folder's, or `None` when the database would
    /// leave it out or no file is there.
    fn read(&self, path: &Path) -> Result<Option<File>, DatabaseError> {
        if self.matcher.classify(path) != Some(FileType::Vendored) {
            return Ok(None);
        }
        let Some(contents) = read_listed(path)? else {
            return Ok(None);
        };
        let name = File::logical_name(&self.workspace, path);
        let file = File::new(Cow::Owned(name), FileType::Vendored, Some(path.to_path_buf()), Cow::Owned(contents));

        Ok((!file.is_companion()).then_some(file))
    }

    /// The patterns that keep the database from reading the plain PHP files the maps serve: those of the configured
    /// `extensions` other than `sharp`, in each served folder.
    fn excludes(&self, extensions: &[&str]) -> Vec<Exclusion<'static>> {
        let extensions: Vec<&str> = if extensions.is_empty() {
            vec!["php"]
        } else {
            extensions.iter().copied().filter(|extension| *extension != "sharp").collect()
        };
        if extensions.is_empty() {
            return Vec::new();
        }

        let extensions = extensions.join(",");
        self.served
            .iter()
            .map(|folder| {
                let folder = folder.strip_prefix(&self.workspace).unwrap_or(folder).to_string_lossy();

                Exclusion::Pattern(Cow::Owned(format!("{folder}/**/*.{{{extensions}}}")))
            })
            .collect()
    }
}

/// The folders under `folder`, `depth` levels down at most, whose plain PHP files the maps alone serve: `folder` itself
/// when none of `held` lies in it, else each such folder among its subfolders, so a held folder and the files beside it
/// load whole.
fn served_folders(folder: &Path, held: &[PathBuf], depth: usize) -> Result<Vec<PathBuf>, DatabaseError> {
    if !held.iter().any(|held| held.starts_with(folder)) {
        return Ok(vec![folder.to_path_buf()]);
    }
    if depth == 0 {
        return Ok(Vec::new());
    }

    let mut served = Vec::new();
    let entries = std::fs::read_dir(folder).map_err(|error| DatabaseError::io(IOAction::OpenFolder, folder, error))?;
    for entry in entries {
        let path = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?.path();
        if path.is_dir() {
            served.extend(served_folders(&path, held, depth - 1)?);
        }
    }
    served.sort();

    Ok(served)
}

/// The folder a `paths`, `includes` or `patches` entry of `workspace` names: the entry up to its first glob part.
fn entry_folder(workspace: &Path, entry: &str) -> PathBuf {
    let literal: PathBuf = Path::new(entry)
        .components()
        .take_while(|part| !part.as_os_str().to_string_lossy().contains(['*', '?', '[', '{']))
        .collect();
    let folder = workspace.join(literal);

    folder.canonicalize().unwrap_or(folder)
}

impl<'cfg> Orchestrator<'cfg> {
    /// Creates a new orchestrator with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The configuration specifying PHP version, paths, tool settings, etc.
    #[must_use]
    pub fn new(config: OrchestratorConfiguration<'cfg>) -> Self {
        Self { config, plugin_registry: OnceLock::new(), context_paths: Vec::new(), autoloader: None }
    }

    /// Reads Composer's autoload maps of the vendor folder when an `includes` entry names it, so the database of
    /// `workspace` [`Self::load_database`] reads with `include_externals` holds only the folder's `.sharp` files and
    /// Composer's own folder, and an analysis reads each of its plain PHP files once it reaches a class the file
    /// declares. The vendor folder is `composer.json`'s `config.vendor-dir`, else `vendor`. A workspace whose vendor
    /// folder has no `composer/autoload_psr4.php`, or whose `includes` don't name it, reads the folder whole.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when `composer.json` or a map can't be read, or a map holds code Composer doesn't
    /// write.
    pub fn autoload_vendor(&mut self, workspace: &Path) -> Result<(), OrchestratorError> {
        let general = |error: mago_composer::autoload::AutoloadError| OrchestratorError::General(error.to_string());
        let workspace = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
        let vendor = vendor_folder(&workspace).map_err(general)?;
        let vendor = vendor.canonicalize().unwrap_or(vendor);
        if !self.config.includes.iter().any(|include| entry_folder(&workspace, include) == vendor) {
            return Ok(());
        }
        let Some(autoload) = Autoload::read(&vendor).map_err(general)? else {
            return Ok(());
        };

        let held: Vec<PathBuf> = self
            .config
            .paths
            .iter()
            .chain(&self.config.includes)
            .chain(&self.config.patches)
            .chain(&self.context_paths)
            .map(|entry| entry_folder(&workspace, entry))
            .filter(|folder| *folder != vendor)
            .chain([vendor.join("composer")])
            .collect();
        let served = served_folders(&vendor, &held, 2)?;
        let matcher = WorkspaceMatcher::from_configuration(&self.database_configuration(&workspace, true))?;
        self.autoloader = Some(Arc::new(Autoloader { autoload, vendor, workspace, matcher, served }));

        Ok(())
    }

    /// The autoload maps [`Self::autoload_vendor`] read, if it read any.
    #[must_use]
    pub fn autoloader(&self) -> Option<Arc<Autoloader>> {
        self.autoloader.clone()
    }

    /// Gets the analyzer plugin registry, initializing it if necessary.
    ///
    /// This method returns a shared reference to the plugin registry used by the analysis service.
    /// If the registry has not been initialized yet, it creates a new one based on the configuration.
    ///
    /// # Returns
    ///
    /// An `Arc` pointing to the `PluginRegistry`.
    pub fn get_analyzer_plugin_registry(&self) -> Arc<PluginRegistry> {
        Arc::clone(self.plugin_registry.get_or_init(|| {
            Arc::new(create_registry_with_plugins(
                &self.config.analyzer_plugins,
                self.config.disable_default_analyzer_plugins,
            ))
        }))
    }

    /// Adds worker-backed analyzer plugins before an analysis service is created.
    pub fn set_external_analyzer(&self, analyzer: ExternalAnalyzer) {
        self.set_external_analyzer_handle(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));
    }

    /// Adds an analyzer that is initializing concurrently with the codebase pipeline.
    pub fn set_external_analyzer_handle(&self, analyzer: Arc<ExternalAnalyzerHandle>) {
        let mut registry =
            create_registry_with_plugins(&self.config.analyzer_plugins, self.config.disable_default_analyzer_plugins);
        registry.set_external_analyzer(analyzer);
        registry.set_project_sources(self.context_paths.clone());
        let result = self.plugin_registry.set(Arc::new(registry));
        debug_assert!(result.is_ok(), "analyzer plugin registry was initialized before external plugins were attached");
    }

    /// Adds additional exclusion patterns to the orchestrator's configuration.
    ///
    /// These patterns will be used when loading the database to exclude files and directories
    /// from scanning. Patterns can be glob patterns (e.g., `"*.tmp"`, `"vendor/*"`) or
    /// direct paths.
    ///
    /// # Arguments
    ///
    /// * `patterns` - A vector of string patterns to exclude from file scanning
    pub fn add_exclude_patterns<T>(&mut self, patterns: impl Iterator<Item = &'cfg T>)
    where
        T: AsRef<str> + 'cfg,
    {
        self.config.excludes.extend(patterns.map(std::convert::AsRef::as_ref));
    }

    /// Sets new source paths, keeping the old ones as context for analysis.
    ///
    /// This method replaces the current source paths with the provided paths. The old
    /// source paths are stored internally so they can provide context during analysis
    /// (e.g., type resolution) without being excluded during lint/format.
    ///
    /// # Arguments
    ///
    /// * `paths` - The new source paths or glob patterns to analyze
    pub fn set_source_paths(&mut self, paths: impl IntoIterator<Item = impl AsRef<str>>) {
        let old_paths = std::mem::take(&mut self.config.paths);

        self.config.paths = paths.into_iter().map(|p| p.as_ref().to_string()).collect();
        self.context_paths.extend(old_paths);
    }

    /// Loads the database by scanning the file system according to the configuration.
    ///
    /// This method scans the workspace directory and builds a database of all PHP files
    /// according to the configured paths, includes, excludes, and extensions. The database
    /// provides fast access to file contents and metadata for all tools. The workspace's `.sharp`
    /// folder, which `mago compile` writes, is never loaded: its Lean package links to source folders.
    ///
    /// # Arguments
    ///
    /// * `workspace` - The root directory of the project to analyze
    /// * `include_externals` - Whether to include files from the `includes` list in the database.
    ///   External files (e.g., vendor dependencies) provide context for analysis but are not
    ///   directly analyzed, linted, or formatted.
    /// * `prelude_database` - An optional pre-existing database to merge with. This is useful
    ///   for including standard library or framework stubs.
    /// * `stdin_override` - When set, the file with the given logical name (workspace-relative path)
    ///   uses the given content instead of being read from disk. Used for editor integrations
    ///   (e.g. `--stdin-input`), so baseline and reporting use the real path.
    ///
    /// # Returns
    ///
    /// Returns a [`Database`] containing all discovered PHP files, or an [`OrchestratorError`]
    /// if the database could not be loaded.
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when filesystem traversal or database initialization fails.
    pub fn load_database<'borrow>(
        &'borrow self,
        workspace: &'cfg Path,
        include_externals: bool,
        prelude_database: Option<Database<'static>>,
        stdin_override: Option<(String, Vec<u8>)>,
    ) -> Result<Database<'cfg>, OrchestratorError>
    where
        'borrow: 'cfg,
    {
        let mut loader = DatabaseLoader::new(self.database_configuration(workspace, include_externals));

        if let Some(prelude_db) = prelude_database {
            loader = loader.with_database(prelude_db);
        }

        if let Some((name, content)) = stdin_override {
            loader = loader.with_stdin_override(name, content);
        }

        let mut result = loader.load().map_err(OrchestratorError::Database)?;

        if let Some(registry) = self.plugin_registry.get() {
            let files = registry
                .external_initialization_files()
                .map_err(|error| OrchestratorError::General(error.to_string()))?;
            result.reserve(files.len());
            for file in files {
                result.add(file);
            }
        }

        Ok(result)
    }

    /// The configuration [`Self::load_database`] reads the files of `workspace` by, the files of the `includes` list
    /// among them when `include_externals` is set, so a watcher can watch those files before they are read.
    pub fn database_configuration<'borrow>(
        &'borrow self,
        workspace: &'borrow Path,
        include_externals: bool,
    ) -> DatabaseConfiguration<'borrow> {
        /// Converts string patterns from the configuration into `Exclusion` types.
        fn create_excludes_from_patterns<'pat>(patterns: &[&'pat str], root: &Path) -> Vec<Exclusion<'pat>> {
            patterns
                .iter()
                .map(|pattern| {
                    if pattern.contains('*') {
                        if let Some(stripped) = pattern.strip_prefix("./") {
                            let rooted_pattern = root.join(stripped).to_string_lossy().into_owned();

                            Exclusion::Pattern(Cow::Owned(rooted_pattern))
                        } else {
                            Exclusion::Pattern(Cow::Borrowed(pattern))
                        }
                    } else {
                        let path = PathBuf::from(pattern);
                        let path_buf = if path.is_absolute() { path } else { root.join(path) };

                        Exclusion::Path(Cow::Owned(path_buf.canonicalize().unwrap_or(path_buf)))
                    }
                })
                .collect()
        }

        let includes: Vec<Cow<'borrow, [u8]>> = if include_externals {
            let active_paths: HashSet<&str> = self.config.paths.iter().map(std::string::String::as_str).collect();

            self.config
                .includes
                .iter()
                .map(std::string::String::as_str)
                .chain(self.context_paths.iter().map(std::string::String::as_str).filter(|p| !active_paths.contains(p)))
                .map(|s| Cow::Borrowed(s.as_bytes()))
                .collect()
        } else {
            Vec::new()
        };

        let mut excludes = create_excludes_from_patterns(&self.config.excludes, workspace);
        excludes.extend(create_excludes_from_patterns(&[COMPILED_FOLDER], workspace));
        if !include_externals {
            let include_strs: Vec<&str> = self.config.includes.iter().map(|s| s.as_ref()).collect();
            excludes.extend(create_excludes_from_patterns(&include_strs, workspace));
        } else if let Some(autoloader) = &self.autoloader {
            excludes.extend(autoloader.excludes(&self.config.extensions));
        }

        let patches = if include_externals {
            self.config.patches.iter().map(|s| Cow::Borrowed(s.as_bytes())).collect::<Vec<Cow<'borrow, [u8]>>>()
        } else {
            Vec::new()
        };

        DatabaseConfiguration {
            workspace: Cow::Borrowed(workspace),
            paths: self.config.paths.iter().map(|s| Cow::Borrowed(s.as_bytes())).collect(),
            includes,
            patches,
            excludes,
            extensions: self.config.extensions.iter().map(|s| Cow::Borrowed(s.as_bytes())).collect(),
            glob: self.config.glob,
        }
    }

    /// Creates a linting service with the current configuration.
    ///
    /// The linting service checks PHP code against a set of rules to identify potential
    /// issues, style violations, and code smells.
    ///
    /// # Arguments
    ///
    /// * `database` - A read-only database handle containing the PHP files to lint
    ///
    /// # Returns
    ///
    /// A [`LintService`] configured with the orchestrator's linter settings and progress bar preferences.
    pub fn get_lint_service(&self, database: ReadDatabase) -> LintService {
        LintService::new(
            database,
            self.config.linter_settings.clone(),
            self.config.parser_settings,
            self.config.use_progress_bars,
        )
    }

    /// Creates an architectural guard service with the current configuration.
    ///
    /// The guard service enforces architectural constraints and layer dependencies in your
    /// codebase, ensuring that code follows the defined architectural rules.
    ///
    /// # Arguments
    ///
    /// * `database` - A read-only database handle containing the PHP files to check
    /// * `codebase` - Metadata about the codebase structure and symbols
    ///
    /// # Returns
    ///
    /// A [`GuardService`] configured with the orchestrator's guard settings and progress bar preferences.
    pub fn get_guard_service(&self, database: ReadDatabase, codebase: CodebaseMetadata) -> GuardService {
        GuardService::new(
            database,
            codebase,
            self.config.guard_settings.clone(),
            self.config.parser_settings,
            self.config.use_progress_bars,
        )
    }

    /// Creates a code formatting service with the current configuration.
    ///
    /// The formatting service formats PHP code according to the configured style settings,
    /// ensuring consistent code style across the codebase.
    ///
    /// # Arguments
    ///
    /// * `database` - A read-only database handle containing the PHP files to format
    ///
    /// # Returns
    ///
    /// A [`FormatService`] configured with the orchestrator's formatter settings, PHP version,
    /// and progress bar preferences.
    pub fn get_format_service(&self, database: ReadDatabase) -> FormatService {
        FormatService::new(
            database,
            self.config.php_version,
            self.config.formatter_settings,
            self.config.parser_settings,
            self.config.use_progress_bars,
        )
    }

    /// Formats a single file according to the configured style settings.
    ///
    /// This is a convenience method for formatting an individual file without requiring
    /// a full database. It creates a temporary format service with an empty database and
    /// uses it to format the provided file.
    ///
    /// # Arguments
    ///
    /// * `file` - The file to format
    ///
    /// # Returns
    ///
    /// - `Ok(FileFormatStatus::Unchanged)` if the file is already properly formatted
    /// - `Ok(FileFormatStatus::Changed(String))` if the file was formatted, containing the new content
    /// - `Ok(FileFormatStatus::FailedToParse(Issue))` if the file couldn't be parsed, with the issue that reports it
    /// - `Err(OrchestratorError)` if formatting failed for other reasons
    ///
    /// # Performance
    ///
    /// This method allocates a new bump arena for each call. For formatting multiple files,
    /// consider using [`get_format_service`](Self::get_format_service) and calling the
    /// service's methods with a reused arena.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when formatting cannot complete (file IO, syntax errors that
    /// prevent emission, etc.).
    pub fn format_file(&self, file: &File) -> Result<FileFormatStatus, OrchestratorError> {
        let service = self.get_format_service(ReadDatabase::empty());

        service.format_file(file)
    }

    /// Formats a single file using a provided bump arena for allocations.
    ///
    /// This method is similar to [`format_file`](Self::format_file) but allows you to
    /// provide your own bump arena for memory allocations. This is more efficient when
    /// formatting multiple files sequentially, as you can reuse and reset the same arena.
    ///
    /// # Arguments
    ///
    /// * `file` - The file to format
    /// * `arena` - A bump allocator for temporary allocations during formatting
    ///
    /// # Returns
    ///
    /// - `Ok(FileFormatStatus::Unchanged)` if the file is already properly formatted
    /// - `Ok(FileFormatStatus::Changed(String))` if the file was formatted, containing the new content
    /// - `Ok(FileFormatStatus::FailedToParse(Issue))` if the file couldn't be parsed, with the issue that reports it
    /// - `Err(OrchestratorError)` if formatting failed for other reasons
    ///
    /// # Performance
    ///
    /// Using this method with a reused arena (resetting it between calls) is significantly
    /// more efficient than calling [`format_file`](Self::format_file) repeatedly, as it
    /// avoids repeated allocator initialization.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when formatting cannot complete (file IO, syntax errors that
    /// prevent emission, etc.).
    pub fn format_file_in(&self, file: &File, arena: &LocalArena) -> Result<FileFormatStatus, OrchestratorError> {
        let service = self.get_format_service(ReadDatabase::empty());

        service.format_file_in(file, arena)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use mago_analyzer::plugin::PluginRegistry;
    use mago_analyzer::settings::Settings as AnalyzerSettings;
    use mago_database::DatabaseReader;
    use mago_database::GlobSettings;
    use mago_formatter::settings::FormatSettings;
    use mago_guard::settings::Settings as GuardSettings;
    use mago_linter::settings::Settings as LinterSettings;
    use mago_php_version::PHPVersion;
    use mago_reporting::IssueCollection;
    use mago_syntax::settings::ParserSettings;

    use super::*;
    use crate::service::incremental_analysis::IncrementalAnalysisService;

    const HEADER: &str = "<?php\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";

    fn write(root: &Path, name: &str, contents: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A project whose `src/Shop.php` reaches `Acme\Money` and calls `acme_loaded()` and `acme_unloaded()`, with the
    /// package `acme/money` in its vendor folder and the maps Composer writes for it: `Money` extends `Amount`, nothing
    /// reaches `Ledger`, and `helpers.php`, which Composer loads up front, declares `acme_loaded()`, while
    /// `functions.php`, which it doesn't load, declares `acme_unloaded()`.
    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();
        write(
            root_path,
            "src/Shop.php",
            "<?php\n\nnamespace App;\n\nuse Acme\\Money;\n\nfunction total(Money $money): int {\n    acme_loaded();\n    acme_unloaded();\n\n    return $money->cents();\n}\n",
        );
        write(
            root_path,
            "vendor/acme/money/src/Amount.php",
            "<?php\n\nnamespace Acme;\n\nclass Amount {\n    public function cents(): int { return 1; }\n}\n",
        );
        write(
            root_path,
            "vendor/acme/money/src/Money.php",
            "<?php\n\nnamespace Acme;\n\nfinal class Money extends Amount {}\n",
        );
        write(root_path, "vendor/acme/money/src/Ledger.php", "<?php\n\nnamespace Acme;\n\nfinal class Ledger {}\n");
        write(root_path, "vendor/acme/money/helpers.php", "<?php\n\nfunction acme_loaded(): void {}\n");
        write(root_path, "vendor/acme/money/functions.php", "<?php\n\nfunction acme_unloaded(): void {}\n");
        write(
            root_path,
            "vendor/composer/autoload_psr4.php",
            &format!("{HEADER}    'Acme\\\\' => array($vendorDir . '/acme/money/src'),\n);\n"),
        );
        write(
            root_path,
            "vendor/composer/autoload_files.php",
            &format!("{HEADER}    'f1' => $vendorDir . '/acme/money/helpers.php',\n);\n"),
        );

        root
    }

    fn orchestrator<'cfg>() -> Orchestrator<'cfg> {
        Orchestrator::new(OrchestratorConfiguration {
            php_version: PHPVersion::PHP85,
            paths: vec!["src".to_owned()],
            includes: vec!["vendor".to_owned()],
            patches: vec![],
            excludes: vec![],
            extensions: vec!["php", "sharp"],
            glob: GlobSettings::default(),
            parser_settings: ParserSettings::default(),
            analyzer_settings: AnalyzerSettings::new(PHPVersion::PHP85),
            linter_settings: LinterSettings::default(),
            guard_settings: GuardSettings::default(),
            formatter_settings: FormatSettings::default(),
            disable_default_analyzer_plugins: false,
            analyzer_plugins: vec![],
            use_progress_bars: false,
            use_colors: false,
        })
    }

    /// The names of `files` in the package `acme`'s folders, in order.
    fn vendor_names(files: impl IntoIterator<Item = Arc<File>>) -> Vec<String> {
        let mut names: Vec<String> = files
            .into_iter()
            .map(|file| String::from_utf8_lossy(&file.name).into_owned())
            .filter(|name| name.starts_with("vendor/acme"))
            .collect();
        names.sort();
        names
    }

    /// The database of `root` as `mago analyze` loads it, and a service that analyzes it with Composer's maps.
    fn service(root: &Path) -> (ReadDatabase, IncrementalAnalysisService) {
        let mut orchestrator = orchestrator();
        orchestrator.autoload_vendor(root).unwrap();
        let database = orchestrator.load_database(root, true, None, None).unwrap();
        let service = IncrementalAnalysisService::new(
            database.read_only(),
            Default::default,
            AnalyzerSettings::new(PHPVersion::PHP85),
            ParserSettings::default(),
            Arc::new(PluginRegistry::with_library_providers()),
        )
        .with_autoloader(orchestrator.autoloader());

        (database.read_only(), service)
    }

    /// Each issue's code and message, in order.
    fn issues(issues: &IssueCollection) -> Vec<String> {
        let mut issues: Vec<String> = issues
            .iter()
            .map(|issue| format!("{} {}", issue.code.clone().unwrap_or_default(), issue.message))
            .collect();
        issues.sort();
        issues
    }

    /// The names of the vendor files the database of `root` holds and the analysis of it read, and its issues.
    fn analyzed(root: &Path) -> (Vec<String>, Vec<String>, Vec<String>) {
        let (database, mut service) = service(root);
        let found = issues(&service.analyze().unwrap().issues);

        (vendor_names(database.files()), vendor_names(service.autoloaded().to_vec()), found)
    }

    #[test]
    fn a_run_reads_the_vendor_class_its_code_reaches_and_the_parent_of_that_class_and_no_other() {
        let root = project();

        let (loaded, read, issues) = analyzed(root.path());

        assert!(loaded.is_empty(), "the database reads no plain PHP vendor file: {loaded:?}");
        assert_eq!(
            read,
            ["vendor/acme/money/helpers.php", "vendor/acme/money/src/Amount.php", "vendor/acme/money/src/Money.php"]
        );
        assert!(!issues.iter().any(|issue| issue.starts_with("non-existent-class")), "{issues:?}");
        assert!(!issues.iter().any(|issue| issue.starts_with("non-existent-method")), "{issues:?}");
    }

    #[test]
    fn a_vendor_function_composer_does_not_load_up_front_is_unknown() {
        let root = project();

        let (_, _, issues) = analyzed(root.path());

        assert_eq!(issues.iter().filter(|issue| issue.starts_with("non-existent-function")).count(), 1, "{issues:?}");
    }

    #[test]
    fn a_vendor_folder_without_composers_maps_is_read_whole() {
        let root = project();
        std::fs::remove_file(root.path().join("vendor/composer/autoload_psr4.php")).unwrap();

        let (loaded, read, _) = analyzed(root.path());

        assert_eq!(loaded.len(), 5, "{loaded:?}");
        assert!(read.is_empty(), "{read:?}");
    }

    #[test]
    fn a_vendor_sharp_file_reaches_the_class_its_extern_line_names() {
        let root = project();
        write(
            root.path(),
            "vendor/acme/money/src/Clock.php",
            "<?php\n\nnamespace Acme;\n\nfinal class Clock {\n    public function now(): int { return 1; }\n}\n",
        );
        write(
            root.path(),
            "vendor/acme/effects/Clocks.sharp",
            "namespace Acme.Effects;\n\nimport Acme.Clock;\n\nextern Clock.now uses none;\n",
        );

        let (loaded, read, issues) = analyzed(root.path());

        assert_eq!(loaded, ["vendor/acme/effects/Clocks.sharp"]);
        assert!(read.contains(&"vendor/acme/money/src/Clock.php".to_owned()), "{read:?} {issues:?}");
        assert!(issues.iter().all(|issue| !issue.contains("Clock")), "{issues:?}");
    }

    #[test]
    fn a_run_from_scratch_after_an_update_reads_the_vendor_files_a_cold_run_reads() {
        let root = project();
        let (database, mut warm) = service(root.path());
        warm.analyze().unwrap();

        warm.update_database(database);
        let again = issues(&warm.analyze().unwrap().issues);

        let (_, read, cold) = analyzed(root.path());
        assert_eq!(vendor_names(warm.autoloaded().to_vec()), read);
        assert_eq!(again, cold);
    }

    #[test]
    fn an_edit_that_reaches_a_vendor_class_the_last_run_did_not_read_reads_it() {
        let root = project();
        let (_, mut warm) = service(root.path());
        warm.analyze().unwrap();
        write(
            root.path(),
            "src/Ledger.php",
            "<?php\n\nnamespace App;\n\nfunction ledger(\\Acme\\Ledger $ledger): int {\n    return $ledger->total();\n}\n",
        );

        let (database, _) = service(root.path());
        warm.update_database(database);
        let after = issues(&warm.analyze_incremental(None).unwrap().issues);

        assert!(vendor_names(warm.autoloaded().to_vec()).contains(&"vendor/acme/money/src/Ledger.php".to_owned()));
        assert!(after.iter().any(|issue| issue.starts_with("non-existent-method")), "{after:?}");
        assert_eq!(after, analyzed(root.path()).2);
    }
}
