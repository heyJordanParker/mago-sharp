use std::sync::Arc;

use foldhash::HashSet;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::analysis_result::LateSymbolReferenceIssueReconciler;
use mago_analyzer::effects::Effects;
use mago_analyzer::error::AnalysisError;
use mago_analyzer::external::FileAnalysisSnapshot;
use mago_analyzer::external::apply_refinements;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::populator::populate_codebase;
use mago_codex::reference::SymbolReferences;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_names::resolver::NameResolver;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file_with_settings;
use mago_syntax::settings::ParserSettings;
use mago_word::WordSet;

use crate::error::OrchestratorError;
use crate::service::body_return::resolve_body_returns;
use crate::service::issue_reconciliation::DeferredIssueReconciler;
use crate::service::issue_reconciliation::drop_follow_on_issues;

pub struct AnalysisService {
    database: ReadDatabase,
    codebase: CodebaseMetadata,
    symbol_references: SymbolReferences,
    settings: Settings,
    parser_settings: ParserSettings,
    plugin_registry: Arc<PluginRegistry>,
}

impl std::fmt::Debug for AnalysisService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnalysisService")
            .field("database", &self.database)
            .field("codebase", &self.codebase)
            .field("symbol_references", &self.symbol_references)
            .field("settings", &self.settings)
            .field("parser_settings", &self.parser_settings)
            .field("plugin_registry", &self.plugin_registry)
            .finish()
    }
}

impl AnalysisService {
    #[must_use]
    pub fn new(
        database: ReadDatabase,
        codebase: CodebaseMetadata,
        symbol_references: SymbolReferences,
        settings: Settings,
        parser_settings: ParserSettings,
        plugin_registry: Arc<PluginRegistry>,
    ) -> Self {
        Self { database, codebase, symbol_references, settings, parser_settings, plugin_registry }
    }

    /// Analyzes a single file synchronously without using parallel processing.
    ///
    /// This method is designed for environments where threading is not available,
    /// such as WebAssembly. It performs static analysis on a single file by:
    /// 1. Parsing the file
    /// 2. Resolving names
    /// 3. Scanning symbols and extending the provided codebase
    /// 4. Populating the codebase (resolving inheritance, traits, etc.)
    /// 5. Running the analyzer
    ///
    /// # Arguments
    ///
    /// * `file_id` - The ID of the file to analyze.
    ///
    /// # Returns
    ///
    /// An `IssueCollection` containing all issues found in the file.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when analysis or an external analyzer operation fails.
    pub fn oneshot(mut self, file_id: FileId) -> Result<IssueCollection, OrchestratorError> {
        let external_session = self.plugin_registry.create_external_analysis_session(self.database.files());
        let Ok(file) = self.database.get(&file_id) else {
            tracing::error!("File with ID {:?} not found in database", file_id);

            return Ok(IssueCollection::default());
        };

        let arena = LocalArena::new();

        let program = parse_file_with_settings(&arena, &file, self.parser_settings);
        let resolved_names = NameResolver::new(&arena).resolve(program);

        let mut issues = IssueCollection::new();
        if program.has_errors() {
            for error in program.errors.iter() {
                issues.push(Issue::from(error));
            }
        }

        let semantics_checker = SemanticsChecker::new(self.settings.version);
        let semantic_issues = semantics_checker.check(&file, program, &resolved_names);
        issues.extend(semantic_issues.iter().cloned());

        let mut user_codebase = scan_program(&arena, &file, program, &resolved_names, self.settings.version);
        let codebase_scan = self
            .plugin_registry
            .external_codebase_scan_plan()
            .map_err(AnalysisError::from)?
            .map(|plan| plan.capture(&file, &user_codebase))
            .transpose()
            .map_err(AnalysisError::from)?
            .flatten();

        self.plugin_registry.prepare_external_analyzer().map_err(AnalysisError::from)?;
        let refinements = self
            .plugin_registry
            .run_external_codebase_scan(codebase_scan.into_iter().collect())
            .map_err(AnalysisError::from)?;
        apply_refinements(refinements, [(file.id, &mut user_codebase)]).map_err(AnalysisError::from)?;
        self.codebase.extend(user_codebase);

        populate_codebase(&mut self.codebase, &mut self.symbol_references, WordSet::default(), HashSet::default());

        let host_files = self.database.files().filter(|file| file.file_type == FileType::Host).collect::<Vec<_>>();
        resolve_body_returns(
            &mut self.codebase,
            &host_files,
            &self.plugin_registry,
            &self.settings,
            self.parser_settings,
            external_session.as_ref(),
        )?;

        let before = self
            .plugin_registry
            .run_external_before_analysis_hooks(&self.codebase, external_session.as_ref())
            .map_err(AnalysisError::from)?;
        issues.extend(before.issues);
        let additional_symbol_references = before.references;
        if !additional_symbol_references.is_empty() {
            self.symbol_references.extend(additional_symbol_references.clone());
        }

        let after_file = self.plugin_registry.has_external_after_file_analysis_hooks().map_err(AnalysisError::from)?;
        let after_analysis = self.plugin_registry.has_external_after_analysis_hooks().map_err(AnalysisError::from)?;
        let node_analysis_requirements =
            self.plugin_registry.external_node_analysis_requirements().map_err(AnalysisError::from)?;

        // Run the analyzer
        let mut analysis_result = AnalysisResult::new(self.symbol_references);
        let mut analyzer =
            Analyzer::new(&arena, &file, &resolved_names, &self.codebase, &self.plugin_registry, self.settings);
        if let Some(requirements) = node_analysis_requirements.as_ref() {
            analyzer = analyzer.with_node_analysis_requirements(requirements);
        }
        analyzer = analyzer.with_deferred_pragmas();
        if let Some(session) = external_session.as_ref() {
            analyzer = analyzer.with_external_analysis_session(session);
        }
        if !additional_symbol_references.is_empty() {
            analyzer = analyzer.with_additional_symbol_references(&additional_symbol_references);
        }

        let artifacts = analyzer.analyze_with_artifacts(program, &mut analysis_result)?;
        analysis_result.issues =
            drop_follow_on_issues(&file, &semantic_issues, std::mem::take(&mut analysis_result.issues));
        analysis_result
            .issues
            .extend(Effects::solve(&self.codebase, &artifacts.effect_summaries).issues(&self.codebase));

        if after_file {
            let reported = self.plugin_registry.run_external_after_file_analysis_hooks(
                &file,
                program,
                &resolved_names,
                &artifacts,
                &self.codebase,
                external_session.as_ref(),
            )?;
            analysis_result.issues.extend(reported.issues);
            let has_late_references = !reported.references_by_file.is_empty();
            for references in reported.references_by_file.into_values() {
                analysis_result.symbol_references.extend(references);
            }

            if has_late_references {
                analysis_result.issues =
                    LateSymbolReferenceIssueReconciler::new(&self.codebase, &analysis_result.symbol_references)
                        .reconcile(std::mem::take(&mut analysis_result.issues));
            }
        }

        analysis_result.issues.extend(self.codebase.take_issues(true));
        let mut pragma_reconciler =
            DeferredIssueReconciler::new(analysis_result.take_deferred_pragmas(), self.database.files());
        analysis_result.issues = pragma_reconciler.reconcile(std::mem::take(&mut analysis_result.issues))?;
        issues.extend(analysis_result.issues.iter().cloned());
        if after_analysis {
            let snapshot = Arc::new(FileAnalysisSnapshot::new(
                &file,
                program,
                &resolved_names,
                &artifacts,
                &self.codebase,
                node_analysis_requirements.as_ref(),
                true,
            )?);
            let mut project_result = AnalysisResult::new(analysis_result.symbol_references);
            project_result.issues = issues.clone();
            let reported = self.plugin_registry.run_external_after_analysis_hooks(
                &project_result,
                &[snapshot],
                &self.codebase,
                external_session.as_ref(),
            )?;
            issues.extend(pragma_reconciler.reconcile(reported)?);
        }

        issues.extend(pragma_reconciler.finish()?);

        Ok(issues)
    }
}
