//! Static analysis command implementation.
//!
//! This module implements the `mago analyze` command, which performs comprehensive
//! static type analysis on PHP codebases to identify type errors, unused code,
//! null safety violations, and other logical issues.
//!
//! # Analysis Process
//!
//! The analyzer follows a multi-phase approach:
//!
//! 1. **Prelude Loading**: Load embedded stubs for PHP built-ins and popular libraries
//! 2. **Database Loading**: Scan and load source files from the workspace
//! 3. **Codebase Model Building**: Construct a complete symbol table and type graph
//! 4. **Analysis**: Perform type checking, control flow analysis, and issue detection
//! 5. **Filtering**: Apply ignore rules and baseline comparisons
//! 6. **Reporting**: Output issues in the configured format
//!
//! # Type Analysis
//!
//! The analyzer performs deep type analysis including:
//!
//! - Type inference and propagation
//! - Type mismatch detection
//! - Null safety checking
//! - Return type validation
//! - Parameter type checking
//! - Property access validation
//!
//! # Stub Support
//!
//! The analyzer includes embedded stubs (`prelude`) containing type information
//! for PHP built-in functions and popular libraries. This enables accurate type
//! checking even for external symbols. Stubs can be disabled with `--no-stubs`
//! for debugging or testing purposes.

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use clap::ColorChoice;
use clap::Parser;
use notify::EventKind;
use notify::RecursiveMode;

use mago_analyzer::code::IssueCode;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::file::FileType;
use mago_database::membership::WorkspaceMatcher;
use mago_database::watcher::DatabaseWatcher;
use mago_database::watcher::WatchOptions;
use mago_database::watcher::start_watcher;
use mago_orchestrator::Orchestrator;
use mago_prelude::Prelude;
use mago_reporting::CompiledIgnoreSet;
use mago_server::Server;
use mago_server::Settings as ServerSettings;

use crate::commands::args::baseline_reporting::BaselineReportingArgs;
use crate::commands::args::substitution::Substitution;
use crate::commands::args::substitution::SubstitutionArgs;
use crate::commands::compile::compile_for_hosts;
use crate::commands::outcome::CommandOutcome;
use crate::commands::stdin_input;
use crate::config::CliOverrides;
use crate::config::Configuration;
use crate::consts::PRELUDE_BYTES;
use crate::error::Error;
use crate::extensions::initialize_external_analyzer;
use crate::extensions::start_external_analyzer;
use crate::utils::create_orchestrator;
use crate::utils::git;

/// The outcome of a watch mode session.
enum WatchOutcome {
    /// A restart was requested due to a configuration, baseline, or dependency file change.
    Restart(String),
}

/// Command for performing static type analysis on PHP code.
///
/// This command runs comprehensive static analysis to detect type errors,
/// unused code, unreachable code paths, and other logical issues that can
/// be found without executing the code.
///
/// # Analysis Features
///
/// The analyzer provides:
///
/// - **Type Checking**: Validates type compatibility across assignments, calls, and returns
/// - **Unused Detection**: Finds unused variables, functions, classes, and expressions
/// - **Dead Code Analysis**: Identifies unreachable code paths
/// - **Null Safety**: Detects potential null pointer dereferences
/// - **Exception Tracking**: Validates thrown exceptions are handled or declared
/// - **Type Inference**: Infers types where not explicitly annotated
///
/// # Stubs and Context
///
/// By default, the analyzer loads embedded stubs for PHP built-ins and popular
/// libraries, providing accurate type information for external symbols. This can
/// be disabled with `--no-stubs` for testing or debugging.
#[derive(Parser, Debug, Default)]
#[command(
    name = "analyze",
    // Alias for the British
    alias = "analyse",
)]
pub struct AnalyzeCommand {
    /// Specific files or directories to report issues for.
    ///
    /// When provided, the analyzer still analyzes every source file in mago.toml,
    /// plus these paths, so checks that compare files see the whole project. It
    /// reports only the issues in the specified files or directories, plus issues
    /// that name no file.
    ///
    /// This is useful for targeted analysis, testing changes, or integrating
    /// with development workflows and CI systems.
    #[arg()]
    pub path: Vec<PathBuf>,

    /// Disable built-in PHP and library stubs for analysis.
    ///
    /// By default, the analyzer uses stubs for built-in PHP functions and popular
    /// libraries to provide accurate type information. Disabling this may result
    /// in more reported issues when external symbols can't be resolved.
    #[arg(long, default_value_t = false)]
    pub no_stubs: bool,

    /// Ignore the `ignore` list from the analyzer configuration.
    ///
    /// Reports every issue the analyzer finds, including codes suppressed by the
    /// configured `ignore` entries. Useful for one-off runs that audit what is
    /// currently being hidden. Inline suppressions in the source are unaffected.
    #[arg(long, default_value_t = false)]
    pub skip_ignores: bool,

    /// Enable watch mode for continuous analysis (experimental).
    ///
    /// When enabled, the analyzer watches the workspace for file changes and
    /// automatically re-runs analysis whenever PHP files are modified,
    /// created, or deleted. This provides instant feedback during development.
    ///
    /// The analyzer also watches configuration files (`mago.toml`), baseline
    /// files, and Composer files (`composer.json`, `composer.lock`) for changes.
    /// When any of these files change, the analyzer automatically restarts
    /// with the updated configuration.
    ///
    /// Press Ctrl+C to stop watching.
    #[arg(long, default_value_t = false, conflicts_with = "substitutions")]
    pub watch: bool,

    /// List all available analyzer issue codes in JSON format.
    ///
    /// Outputs a JSON array of all issue code strings that the analyzer
    /// can report. Useful for tooling integration and documentation.
    #[arg(long, conflicts_with_all = ["path", "no_stubs", "skip_ignores", "watch", "reporting_target", "reporting_format"])]
    pub list_codes: bool,

    /// Only analyze files that are staged in git.
    ///
    /// This flag is designed for git pre-commit hooks. It will find all PHP files
    /// currently staged for commit and analyze only those files.
    ///
    /// Fails if not in a git repository.
    #[arg(long, conflicts_with_all = ["path", "list_codes", "watch", "substitutions"])]
    pub staged: bool,

    /// Read the file content from stdin and use the given path for baseline and reporting.
    ///
    /// Intended for editor integrations: pipe unsaved buffer content and pass the real file path
    /// so baseline entries and issue locations use the correct path.
    #[arg(long, conflicts_with_all = ["list_codes", "watch", "staged"])]
    pub stdin_input: bool,

    /// Build the codebase and run the extensions' before-analysis hooks, then analyze no file.
    ///
    /// Reports only the issues those hooks raise, without the baseline. Useful for extensions
    /// that write what they read from the codebase, such as a discovery manifest.
    #[arg(long, conflicts_with_all = ["list_codes", "watch", "staged", "stdin_input"])]
    pub scan_only: bool,

    /// Hidden flag to catch `--only` usage and show a helpful error.
    #[arg(long, hide = true, num_args = 1..)]
    pub only: Vec<String>,

    /// Arguments related to reporting issues with baseline support.
    #[clap(flatten)]
    pub baseline_reporting: BaselineReportingArgs,

    /// File-content substitutions (`--substitute ORIG=TEMP`).
    #[clap(flatten)]
    pub substitution: SubstitutionArgs,
}

impl AnalyzeCommand {
    /// Executes the static analysis process.
    ///
    /// This method orchestrates the complete analysis workflow:
    ///
    /// 1. **Load Prelude**: Decode embedded stubs for PHP built-ins (unless `--no-stubs`)
    /// 2. **Create Orchestrator**: Initialize with configuration and color settings
    /// 3. **Apply Overrides**: Use `path` argument if provided to override config paths
    /// 4. **Load Database**: Scan workspace and include external files for context
    /// 5. **Validate Files**: Ensure at least one host file exists to analyze
    /// 6. **Create Service**: Initialize analysis service with database and prelude
    /// 7. **Run Analysis**: Perform type checking and issue detection
    /// 8. **Filter Issues**: Apply ignore rules from configuration
    /// 9. **Process Results**: Report issues through baseline processor
    ///
    /// # Arguments
    ///
    /// * `configuration` - The loaded configuration containing analyzer settings
    /// * `color_choice` - Whether to use colored output
    ///
    /// # Returns
    ///
    /// - `Ok(ExitCode::SUCCESS)` if analysis completed successfully
    /// - `Err(Error)` if database loading, analysis, or reporting failed
    ///
    /// # File Types
    ///
    /// The analyzer distinguishes between:
    /// - **Host files**: Source files to analyze (from configured paths)
    /// - **External files**: Context files (from includes) that provide type information
    ///
    /// Only host files are analyzed for issues; external files only contribute to
    /// the symbol table and type graph.
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<CommandOutcome, Error> {
        if !self.only.is_empty() {
            eprintln!("error: the `--only` flag is not available for the analyzer.");
            eprintln!();
            eprintln!("  Unlike the linter, the analyzer is not rule-based and does not support");
            eprintln!("  selectively enabling individual checks.");
            eprintln!();
            eprintln!("  To filter the output to specific issue codes, use `--retain-code`:");
            eprintln!();
            eprintln!("    mago analyze --retain-code {}", self.only.join(" --retain-code "));
            eprintln!();
            eprintln!("  This runs the full analysis but only reports issues matching the given codes.");
            eprintln!("  Use `mago analyze --list-codes` to see all available codes.");

            return Ok(ExitCode::FAILURE.into());
        }

        if self.list_codes {
            let codes: Vec<_> = IssueCode::all().iter().map(|c| c.as_str()).collect();

            println!("{}", serde_json::to_string_pretty(&codes)?);

            return Ok(ExitCode::SUCCESS.into());
        }

        // Check if watch mode is enabled early, since it needs a restart loop
        if self.watch {
            return self.run_watch_loop(configuration, color_choice).map(CommandOutcome::from);
        }

        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        let command_start = trace_enabled.then(Instant::now);

        let orchestrator_init_start = trace_enabled.then(Instant::now);
        let substitutions = self.substitution.resolve()?;
        let substitution_excludes: Vec<String> =
            substitutions.iter().map(|s| s.original.to_string_lossy().into_owned()).collect();

        let mut orchestrator = create_orchestrator(&configuration, color_choice, false, true, false);
        analyze_the_whole_workspace_without_paths(&mut orchestrator);
        orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
        orchestrator.add_exclude_patterns(substitution_excludes.iter());
        for substitution in &substitutions {
            orchestrator.config.paths.push(substitution.temporary.to_string_lossy().into_owned());
        }

        let stdin_override =
            stdin_input::resolve_stdin_override(self.stdin_input, &self.path, &configuration.source.workspace)?;

        let workspace = configuration.source.workspace.as_path();
        let mut scope = None;
        if !self.stdin_input && self.staged {
            let staged_paths = git::get_staged_file_paths(workspace)?;
            if staged_paths.is_empty() {
                tracing::info!("No staged files to analyze.");
                return Ok(ExitCode::SUCCESS.into());
            }

            if self.baseline_reporting.reporting.fix {
                git::ensure_staged_files_are_clean(workspace, &staged_paths)?;
            }

            scope = Some(scope_to(&mut orchestrator, workspace, &staged_paths, &substitutions)?);
        } else if !self.path.is_empty() {
            scope = Some(scope_to(&mut orchestrator, workspace, &self.path, &substitutions)?);
        }

        let orchestrator_init_duration = orchestrator_init_start.map(|s| s.elapsed());
        let load_inputs_start = trace_enabled.then(Instant::now);
        let mut prelude_duration = None;
        let mut load_database_duration = None;
        let (extension_hosts, _) = compile_for_hosts(&configuration)?;
        let external_analyzer = start_external_analyzer(
            &extension_hosts,
            configuration.php_version,
            configuration.threads,
            &configuration.analyzer.plugins,
            configuration.analyzer.disable_default_plugins,
        );
        if let Some(external_analyzer) = external_analyzer {
            orchestrator.set_external_analyzer_handle(external_analyzer);
        }

        let (prelude_database, database) = rayon::join(
            || {
                let start = trace_enabled.then(Instant::now);
                let prelude_database = self.prelude_database();
                prelude_duration = start.map(|s| s.elapsed());
                prelude_database
            },
            || {
                let start = trace_enabled.then(Instant::now);
                let database = orchestrator.load_database(&configuration.source.workspace, true, None, stdin_override);
                load_database_duration = start.map(|s| s.elapsed());
                database
            },
        );

        let mut database = database?;
        database.merge_base(prelude_database);
        let load_inputs_duration = load_inputs_start.map(|s| s.elapsed());

        if !database.files().any(|f| f.file_type == FileType::Host) {
            tracing::warn!("No files found to analyze.");

            return Ok(ExitCode::SUCCESS.into());
        }

        let service_run_start = trace_enabled.then(Instant::now);
        let mut server = Server::new(database.into_static(), self.prelude(), server_settings(&orchestrator));
        if self.scan_only {
            server = server.scan_only();
        }
        if let Some(scope) = scope {
            server = server.scoped_to(scope);
        }
        let mut issues = server.analyze()?.issues;
        let service_run_duration = service_run_start.map(|s| s.elapsed());
        let report_start = trace_enabled.then(Instant::now);
        let ignore_set = self.compile_ignore_set(&configuration);
        let database = server.database_mut();

        issues.filter_out_ignored(&ignore_set, |file_id| {
            database.get_ref(&file_id).ok().map(|f| String::from_utf8_lossy(&f.name).into_owned())
        });

        let baseline = if self.scan_only { None } else { configuration.analyzer.baseline.as_deref() };
        let baseline_variant = configuration.analyzer.baseline_variant;
        let processor = self.baseline_reporting.get_processor(
            color_choice,
            baseline,
            baseline_variant,
            configuration.editor_url.clone(),
            configuration.analyzer.minimum_fail_level,
            self.staged || !self.path.is_empty() || self.stdin_input,
        );

        let (exit_code, changed_file_ids) = processor.process_issues(&orchestrator, database, issues)?;
        let outcome = CommandOutcome::with_changes(exit_code, database, changed_file_ids.iter().copied())?;
        let report_duration = report_start.map(|s| s.elapsed());

        if self.staged && !changed_file_ids.is_empty() {
            git::stage_files(&configuration.source.workspace, database, changed_file_ids)?;
        }

        let drop_server_start = trace_enabled.then(Instant::now);
        drop(server);
        let drop_server_duration = drop_server_start.map(|s| s.elapsed());

        let drop_orchestrator_start = trace_enabled.then(Instant::now);
        drop(orchestrator);
        let drop_orchestrator_duration = drop_orchestrator_start.map(|s| s.elapsed());

        if let Some(start) = command_start {
            tracing::trace!("Orchestrator initialized in {:?}.", orchestrator_init_duration.unwrap_or_default());
            tracing::trace!("Prelude decoded in {:?} (concurrent task).", prelude_duration.unwrap_or_default());
            tracing::trace!("Database loaded in {:?} (concurrent task).", load_database_duration.unwrap_or_default());
            tracing::trace!("Prelude and database loaded in {:?}.", load_inputs_duration.unwrap_or_default());
            tracing::trace!("Analysis server ran in {:?}.", service_run_duration.unwrap_or_default());
            tracing::trace!("Issues filtered and reported in {:?}.", report_duration.unwrap_or_default());
            tracing::trace!("Server dropped in {:?}.", drop_server_duration.unwrap_or_default());
            tracing::trace!("Orchestrator dropped in {:?}.", drop_orchestrator_duration.unwrap_or_default());
            tracing::trace!("Analyze command finished in {:?}.", start.elapsed());
        }

        Ok(outcome)
    }

    /// Compiles the configured ignore entries, or an empty set with `--skip-ignores`.
    fn compile_ignore_set(&self, configuration: &Configuration) -> CompiledIgnoreSet {
        if self.skip_ignores {
            return CompiledIgnoreSet::default();
        }

        CompiledIgnoreSet::compile(&configuration.analyzer.ignore, configuration.source.glob.to_database_settings())
    }

    /// Decodes the prelude's stub files, which join the workspace database, or none with `--no-stubs`.
    ///
    /// The server decodes the prelude's codebase itself, through [`Self::prelude`].
    fn prelude_database(&self) -> Database<'static> {
        if self.no_stubs {
            Prelude::default().database
        } else {
            Prelude::decode_database(PRELUDE_BYTES).expect("Failed to decode embedded prelude")
        }
    }

    /// The function the server calls to decode the prelude's codebase for each analysis from scratch.
    fn prelude(&self) -> fn() -> (CodebaseMetadata, SymbolReferences) {
        if self.no_stubs { Default::default } else { decode_prelude }
    }

    /// Wraps watch mode in a restart loop.
    ///
    /// When configuration files, baseline files, or Composer files change,
    /// the watch session restarts with the reloaded configuration. The watcher of those files is live before each load
    /// of the configuration, so a change made after `mago` first read it restarts the watch too.
    fn run_watch_loop(&self, configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        let (mut restart_watcher, mut configuration) = watch_and_reload(configuration)?;
        loop {
            let mut orchestrator = create_orchestrator(&configuration, color_choice, false, false, true);
            analyze_the_whole_workspace_without_paths(&mut orchestrator);
            orchestrator.add_exclude_patterns(configuration.analyzer.excludes.iter());
            let scope = if self.path.is_empty() {
                None
            } else {
                Some(scope_to(&mut orchestrator, &configuration.source.workspace, &self.path, &[])?)
            };

            // The watcher reports each change from here on before the hosts' compile or the analysis reads a file, so
            // an edit made after a file is read reaches it, and one made before is in what was read.
            tracing::info!("Starting watch mode. Press Ctrl+C to stop.");
            let workspace = &configuration.source.workspace;
            let mut watcher =
                DatabaseWatcher::new(Database::new(orchestrator.database_configuration(workspace, true).into_static()));
            watcher.watch(WatchOptions { poll_interval: Some(Duration::from_millis(500)), ..Default::default() })?;

            let (extension_hosts, _) = compile_for_hosts(&configuration)?;
            if let Some(external_analyzer) = initialize_external_analyzer(
                &extension_hosts,
                configuration.php_version,
                configuration.threads,
                &configuration.analyzer.plugins,
                configuration.analyzer.disable_default_plugins,
            )
            .map_err(|error| mago_orchestrator::OrchestratorError::General(error.to_string()))?
            {
                orchestrator.set_external_analyzer(external_analyzer);
            }
            let database =
                orchestrator.load_database(workspace, true, Some(self.prelude_database()), None)?.into_static();
            *watcher.database_mut() = database;

            match self.run_watch_mode(
                orchestrator,
                &configuration,
                color_choice,
                scope,
                watcher,
                &restart_watcher.reasons,
            )? {
                WatchOutcome::Restart(reason) => {
                    tracing::info!("Restarting analysis: {reason}");

                    // The restart watcher stops here, once the next one is live.
                    (restart_watcher, configuration) = watch_and_reload(configuration)?;
                }
            }
        }
    }

    /// Runs in watch mode, continuously monitoring for file changes and re-analyzing.
    ///
    /// `watcher` holds the files the analysis reads and reports each change to them, and `restart_receiver` each change
    /// to the configuration, baseline and Composer files, for which it returns `WatchOutcome::Restart` so the caller
    /// can reload configuration and restart.
    fn run_watch_mode(
        &self,
        orchestrator: Orchestrator<'_>,
        configuration: &Configuration,
        color_choice: ColorChoice,
        scope: Option<WorkspaceMatcher>,
        mut watcher: DatabaseWatcher<'static>,
        restart_receiver: &mpsc::Receiver<String>,
    ) -> Result<WatchOutcome, Error> {
        let mut server = Server::new(watcher.database().clone(), self.prelude(), server_settings(&orchestrator));
        if let Some(scope) = scope {
            server = server.scoped_to(scope);
        }

        tracing::info!("Watching {} for changes...", configuration.source.workspace.display());
        tracing::info!("Running initial analysis...");

        let mut issues = server.analyze()?.issues;

        let ignore_set = self.compile_ignore_set(configuration);

        issues.filter_out_ignored(&ignore_set, |file_id| {
            server.database().get_ref(&file_id).ok().map(|f| String::from_utf8_lossy(&f.name).into_owned())
        });

        let baseline = configuration.analyzer.baseline.as_deref();
        let baseline_variant = configuration.analyzer.baseline_variant;

        let processor = self.baseline_reporting.get_processor(
            color_choice,
            baseline,
            baseline_variant,
            configuration.editor_url.clone(),
            configuration.analyzer.minimum_fail_level,
            self.staged || !self.path.is_empty() || self.stdin_input,
        );

        watcher.with_database_mut(|database| processor.process_issues(&orchestrator, database, issues).map(|_| ()))?;

        tracing::info!("Initial analysis complete. Watching for changes...");

        loop {
            // Check for restart triggers (config, baseline, composer changes).
            if let Ok(reason) = restart_receiver.try_recv() {
                return Ok(WatchOutcome::Restart(reason));
            }

            let changed_file_ids = watcher.wait()?;
            if changed_file_ids.is_empty() {
                continue;
            }

            tracing::info!("Detected {} file change(s), re-analyzing...", changed_file_ids.len());

            *server.database_mut() = watcher.database().clone().into_static();
            // A host's worker keeps each class it loaded, as PHP declares a class once, so a change the hosts' compile
            // takes in starts every worker again, as a dev server restarts a worker when a module it loaded changes.
            if compile_for_hosts(configuration)?.1 {
                return Ok(WatchOutcome::Restart("a PHP# file the extension hosts load changed".to_owned()));
            }

            let mut issues = server.analyze_incremental(&changed_file_ids)?.issues;
            issues.filter_out_ignored(&ignore_set, |file_id| {
                server.database().get_ref(&file_id).ok().map(|f| String::from_utf8_lossy(&f.name).into_owned())
            });

            watcher
                .with_database_mut(|database| processor.process_issues(&orchestrator, database, issues).map(|_| ()))?;

            tracing::info!("Analysis complete. Watching for changes...");
        }
    }
}

/// Projects the orchestrator's resolved configuration down to what an analysis server needs.
pub(crate) fn server_settings(orchestrator: &Orchestrator<'_>) -> ServerSettings {
    ServerSettings {
        parser: orchestrator.config.parser_settings,
        analyzer: orchestrator.config.analyzer_settings.clone(),
        plugin_registry: orchestrator.get_analyzer_plugin_registry(),
        use_progress_bars: orchestrator.config.use_progress_bars,
    }
}

/// Makes an empty `[source] paths` mean the whole workspace, as the configuration documents.
pub(crate) fn analyze_the_whole_workspace_without_paths(orchestrator: &mut Orchestrator<'_>) {
    if orchestrator.config.paths.is_empty() {
        orchestrator.config.paths.push(".".to_owned());
    }
}

/// Adds the `named` files to the host paths and returns the scope that reports only their issues.
///
/// The rest of the workspace stays loaded, so rules that judge files against each other still see
/// it. A named file that `--substitute` replaces is scoped by its replacement.
fn scope_to(
    orchestrator: &mut Orchestrator<'_>,
    workspace: &Path,
    named: &[PathBuf],
    substitutions: &[Substitution],
) -> Result<WorkspaceMatcher, Error> {
    let paths = named
        .iter()
        .map(|path| {
            let substituted = workspace.join(path).canonicalize().ok().and_then(|named| {
                substitutions
                    .iter()
                    .find(|substitution| substitution.original.canonicalize().is_ok_and(|original| original == named))
            });

            substituted.map_or(path, |substitution| &substitution.temporary).to_string_lossy().into_owned()
        })
        .collect::<Vec<_>>();

    let scope = WorkspaceMatcher::from_configuration(&DatabaseConfiguration {
        workspace: Cow::Borrowed(workspace),
        paths: paths.iter().map(|path| Cow::Borrowed(path.as_bytes())).collect(),
        includes: Vec::new(),
        patches: Vec::new(),
        excludes: Vec::new(),
        extensions: orchestrator
            .config
            .extensions
            .iter()
            .map(|extension| Cow::Borrowed(extension.as_bytes()))
            .collect(),
        glob: orchestrator.config.glob,
    })?;
    orchestrator.config.paths.extend(paths);

    Ok(scope)
}

/// Decodes the embedded prelude's codebase and symbol references.
pub(crate) fn decode_prelude() -> (CodebaseMetadata, SymbolReferences) {
    let Prelude { metadata, symbol_references, .. } =
        Prelude::decode(PRELUDE_BYTES).expect("Failed to decode embedded prelude");

    (metadata, symbol_references)
}

/// A watcher of the non-PHP files whose change restarts watch mode, and the reason it gives for each change.
struct RestartWatcher {
    /// Watches while it lives.
    _watcher: Box<dyn notify::Watcher + Send>,
    /// The [`restart_files`] it watches.
    files: Vec<(PathBuf, &'static str)>,
    /// A human-readable reason for each change to one of the files.
    reasons: mpsc::Receiver<String>,
}

/// Starts the restart watcher of `configuration`'s files, then loads the configuration again, so a change to one of
/// them that the load misses restarts the watch. While the loaded configuration names other files, it starts the
/// watcher of those and loads again. Each watcher stops once the next is live.
fn watch_and_reload(mut configuration: Configuration) -> Result<(RestartWatcher, Configuration), Error> {
    let mut restart_watcher = setup_restart_watcher(&configuration)?;
    loop {
        let CliOverrides { workspace, config, php_version, threads, allow_unsupported_php_version, no_version_check } =
            configuration.cli_overrides.clone();

        let mut reloaded = match Configuration::load(
            workspace,
            config.as_deref(),
            php_version,
            threads,
            allow_unsupported_php_version,
            no_version_check,
        ) {
            Ok(reloaded) => reloaded,
            Err(e) => {
                tracing::error!("Failed to reload configuration: {e}");
                tracing::info!("Continuing with previous configuration.");

                return Ok((restart_watcher, configuration));
            }
        };
        if configuration.no_extensions {
            reloaded.disable_extensions();
        }
        if restart_files(&reloaded) == restart_watcher.files {
            return Ok((restart_watcher, reloaded));
        }

        restart_watcher = setup_restart_watcher(&reloaded)?;
        configuration = reloaded;
    }
}

/// The non-PHP files whose change restarts watch mode, each with a label for the reason:
/// - Configuration files (`mago.toml`, `mago.dist.toml`, etc.)
/// - Baseline file (if configured)
/// - `composer.json` and `composer.lock` (if present)
fn restart_files(configuration: &Configuration) -> Vec<(PathBuf, &'static str)> {
    let workspace = configuration.source.workspace.as_path();
    let mut watch_files: Vec<(PathBuf, &'static str)> = Vec::new();

    if let Some(config_file) = &configuration.config_file {
        watch_files.push((config_file.clone(), "configuration file"));
    } else {
        // No config file was found, watch all possible locations so we detect creation of a new config file.
        for name in ["mago", "mago.dist"] {
            for ext in ["toml", "yaml", "yml", "json"] {
                watch_files.push((workspace.join(format!("{name}.{ext}")), "configuration file"));
            }
        }
    }

    if let Some(baseline) = &configuration.analyzer.baseline {
        let path = if baseline.is_absolute() { baseline.clone() } else { workspace.join(baseline) };

        watch_files.push((path, "baseline file"));
    }

    watch_files.push((workspace.join("composer.json"), "composer.json"));
    watch_files.push((workspace.join("composer.lock"), "composer.lock"));

    watch_files
        .into_iter()
        .map(|(path, label)| (if path.is_absolute() { path } else { workspace.join(path) }, label))
        .collect()
}

/// Starts a watcher of `configuration`'s [`restart_files`], which gives a human-readable reason for each change made
/// to one after it returns, as [`start_watcher`] says.
fn setup_restart_watcher(configuration: &Configuration) -> Result<RestartWatcher, Error> {
    let workspace = configuration.source.workspace.as_path();
    let (tx, rx) = mpsc::channel();
    let files = restart_files(configuration);
    let file_labels = files.clone();

    let mut watched_dirs: Vec<(PathBuf, RecursiveMode)> = Vec::new();
    for (path, label) in &files {
        let watch_dir = path.parent().unwrap_or(workspace);
        if watched_dirs.iter().any(|(watched, _)| watched == watch_dir) {
            continue;
        }
        if watch_dir.is_dir() {
            watched_dirs.push((watch_dir.to_path_buf(), RecursiveMode::NonRecursive));
            tracing::debug!("Watching {label}: {}", path.display());
        } else {
            tracing::warn!("Could not watch {label} at {}: the folder does not exist", watch_dir.display());
        }
    }

    let watcher = start_watcher(
        move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else {
                return;
            };

            if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)) {
                for event_path in &event.paths {
                    for (watched_path, label) in &file_labels {
                        let matches = event_path
                            .canonicalize()
                            .ok()
                            .and_then(|canon| watched_path.canonicalize().ok().map(|wc| canon == wc))
                            .unwrap_or_else(|| event_path == watched_path);

                        if matches {
                            let _ = tx.send(format!("{label} changed ({})", event_path.display()));
                            return;
                        }
                    }
                }
            }
        },
        &watched_dirs,
        Duration::from_millis(500),
    )?;

    Ok(RestartWatcher { _watcher: watcher, files, reasons: rx })
}

#[cfg(test)]
mod tests {
    use mago_php_version::PHPVersion;

    use super::*;

    /// A change to the configuration file made before its restart watcher is live, as one saved while `mago` starts,
    /// is in the configuration `watch_and_reload` returns, and the watcher it returns watches the baseline that the
    /// change names, which the configuration `mago` read first did not.
    #[test]
    fn a_configuration_change_made_before_the_restart_watcher_is_live_is_loaded_and_watched() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("mago.toml");
        std::fs::write(&file, "php-version = \"8.4\"\n").unwrap();
        let configuration = Configuration::load(Some(root.clone()), Some(&file), None, None, false, true).unwrap();
        std::fs::write(&file, "php-version = \"8.4\"\n\n[analyzer]\nbaseline = \"baseline.toml\"\n").unwrap();

        let (restart_watcher, configuration) = watch_and_reload(configuration).unwrap();

        let baseline = root.join("baseline.toml");
        assert_eq!(configuration.analyzer.baseline.as_deref(), Some(baseline.as_path()));
        assert!(restart_watcher.files.contains(&(baseline, "baseline file")), "{:?}", restart_watcher.files);
    }

    /// `--no-extensions` keeps the extension hosts off when watch mode loads the configuration again.
    #[test]
    fn a_reload_keeps_the_extension_hosts_off_under_no_extensions() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("mago.toml");
        std::fs::write(
            &file,
            "php-version = \"8.4\"\n\n[extension-hosts.logger]\nworkers = 1\ncommand = [\"php\", \"host.php\"]\n",
        )
        .unwrap();
        let mut configuration = Configuration::load(Some(root.clone()), Some(&file), None, None, false, true).unwrap();
        assert!(configuration.extension_hosts.contains_key("logger"));
        configuration.disable_extensions();

        let (_restart_watcher, configuration) = watch_and_reload(configuration).unwrap();

        assert!(configuration.extension_hosts.is_empty(), "{:?}", configuration.extension_hosts.keys());
    }

    /// A reload reads each value the edited configuration file gives, as a fresh start does, while a `--php-version`
    /// passed on the command line still wins over the file.
    #[test]
    fn a_reload_reads_the_edited_php_version_and_threads_and_keeps_a_command_line_php_version() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("mago.toml");

        temp_env::with_vars([("MAGO_PHP_VERSION", None::<&str>), ("MAGO_THREADS", None)], || {
            std::fs::write(&file, "php-version = \"8.3\"\nthreads = 2\n").unwrap();
            let configuration = Configuration::load(Some(root.clone()), Some(&file), None, None, false, true).unwrap();
            std::fs::write(&file, "php-version = \"8.5\"\nthreads = 3\n").unwrap();

            let (_restart_watcher, configuration) = watch_and_reload(configuration).unwrap();

            assert_eq!((configuration.php_version, configuration.threads), (PHPVersion::PHP85, 3));

            let configuration =
                Configuration::load(Some(root.clone()), Some(&file), Some(PHPVersion::PHP83), None, false, true)
                    .unwrap();
            std::fs::write(&file, "php-version = \"8.4\"\nthreads = 4\n").unwrap();

            let (_restart_watcher, configuration) = watch_and_reload(configuration).unwrap();

            assert_eq!((configuration.php_version, configuration.threads), (PHPVersion::PHP83, 4));
        });
    }

    /// A reload reads again the workspace, `allow-unsupported-php-version` and `no-version-check` the edited
    /// configuration file gives, when the command line set none of them.
    #[test]
    fn a_reload_reads_the_edited_workspace_and_version_checks() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let file = root.join("mago.toml");
        for workspace in ["first", "second"] {
            std::fs::create_dir(root.join(workspace)).unwrap();
        }

        temp_env::with_vars(
            [("MAGO_ALLOW_UNSUPPORTED_PHP_VERSION", None::<&str>), ("MAGO_NO_VERSION_CHECK", None)],
            || {
                std::fs::write(
                    &file,
                    format!(
                        "allow-unsupported-php-version = true\nno-version-check = true\n\n[source]\nworkspace = {:?}\n",
                        root.join("first")
                    ),
                )
                .unwrap();
                let configuration = Configuration::load(None, Some(&file), None, None, false, false).unwrap();
                std::fs::write(&file, format!("[source]\nworkspace = {:?}\n", root.join("second"))).unwrap();

                let (_restart_watcher, configuration) = watch_and_reload(configuration).unwrap();

                assert_eq!(
                    (
                        configuration.source.workspace,
                        configuration.allow_unsupported_php_version,
                        configuration.no_version_check
                    ),
                    (root.join("second"), false, false)
                );
            },
        );
    }
}
