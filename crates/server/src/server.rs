//! The single-workspace [`Server`]: the transport-agnostic core that owns one
//! workspace's file database and analysis service, and answers queries against them.

use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;

use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_analyzer::effects::Effects;
use mago_analyzer::external::ReturnTypeRequests;
use mago_analyzer::plugin::PluginRegistry;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_orchestrator::error::OrchestratorError;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_orchestrator::service::incremental_analysis::Scans;
use mago_orchestrator::service::incremental_analysis::compile::Compiled;
use mago_orchestrator::service::incremental_analysis::compile::DispatcherModule;
use mago_orchestrator::service::incremental_analysis::compile::ExternAttributes;
use mago_orchestrator::service::incremental_analysis::compile::ExternEffects;
use mago_orchestrator::service::incremental_analysis::compile::Family;
use mago_orchestrator::service::incremental_analysis::compile::Folder;
use mago_orchestrator::service::incremental_analysis::compile::Listener;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Library;
use mago_sharp_lean::Reach;
use mago_sharp_lean::Translation;
use mago_word::Word;

use crate::error::ServerError;
use crate::settings::Settings;

/// A transport-agnostic backend for a single workspace.
///
/// Owns the workspace's [`Database`] and an [`IncrementalAnalysisService`]. It
/// performs no I/O: callers supply file contents as in-memory buffers and the
/// server answers queries in terms of [`FileId`]s.
pub struct Server {
    database: Database<'static>,
    service: IncrementalAnalysisService,
    scope: Option<Scope>,
}

/// Whether a file is one [`Server::scoped_to`] reports in.
type Scope = Box<dyn Fn(&File) -> bool + Send + Sync>;

impl Server {
    /// Build a server for one workspace from an already-loaded file database,
    /// the function that decodes the prelude, and resolved [`Settings`].
    ///
    /// Construction performs no analysis; call [`Server::analyze`] for the
    /// initial pass. Each analysis from scratch decodes the prelude afresh.
    #[must_use]
    pub fn new(
        database: Database<'static>,
        prelude: fn() -> (CodebaseMetadata, SymbolReferences),
        settings: Settings,
    ) -> Self {
        let Settings { parser, analyzer, plugin_registry, use_progress_bars, autoloader } = settings;

        let service = IncrementalAnalysisService::new(database.read_only(), prelude, analyzer, parser, plugin_registry)
            .with_progress_bars(use_progress_bars)
            .with_autoloader(autoloader);

        Self { database, service, scope: None }
    }

    /// Build the codebase and run the before-analysis hooks, then analyze no
    /// file: every analysis reports only the issues those hooks raise and each
    /// file's parse errors.
    #[must_use]
    pub fn scan_only(mut self) -> Self {
        self.service = self.service.scan_only();
        self
    }

    /// Keep, from each analysis from scratch, what it gives the lowering of each PHP# file, so [`Server::compile`]
    /// lowers each file from the analysis it already made instead of analyzing it again.
    #[must_use]
    pub fn keeping_lowering_inputs(mut self) -> Self {
        self.service = self.service.keeping_lowering_inputs();
        self
    }

    /// Keep, from each analysis from scratch, the first scan of each file, which [`Server::take_scans`] hands to the
    /// next analysis of the same files in the run, so that one parses and scans only the files that differ.
    #[must_use]
    pub fn keeping_scans(mut self) -> Self {
        self.service = self.service.keeping_scans();
        self
    }

    /// The scans the last analysis from scratch kept, which the server no longer holds.
    pub fn take_scans(&mut self) -> Scans {
        self.service.take_scans()
    }

    /// Give the first analysis `scans`, another analysis's, which it takes for each file it holds unchanged and scans
    /// with the same settings, instead of scanning the file again.
    #[must_use]
    pub fn with_scans(mut self, scans: Scans) -> Self {
        self.service = self.service.with_scans(scans);
        self
    }

    /// Leave the extension host issues of `package_files` out of each analysis: the packages' `.sharp` files a compile
    /// checks as files of the workspace, which a host reports on no more than on any other file outside the
    /// configuration's `paths`.
    #[must_use]
    pub fn with_package_files(mut self, package_files: impl IntoIterator<Item = FileId>) -> Self {
        self.service = self.service.with_package_files(package_files);
        self
    }

    /// Analyze the whole workspace, but report only the issues in the files `scope` holds, plus the
    /// issues that name no file. An issue is reported in its primary annotation's file, else in its
    /// first annotation's. The node-analysis hooks report only in the file they inspect, so they run
    /// only in the files `scope` holds.
    #[must_use]
    pub fn scoped_to(mut self, scope: impl Fn(&File) -> bool + Send + Sync + 'static) -> Self {
        self.scope = Some(Box::new(scope));
        self
    }

    /// Borrow the workspace file database.
    #[must_use]
    pub fn database(&self) -> &Database<'static> {
        &self.database
    }

    /// The codebase of the last analysis, which stays empty until the first one.
    #[must_use]
    pub fn codebase(&self) -> &CodebaseMetadata {
        self.service.codebase()
    }

    /// The references between the symbols of the last analysis, as each property write with the member or the file
    /// that writes it, which `mago convert` reads to find who sets a property.
    #[must_use]
    pub fn symbol_references(&self) -> &SymbolReferences {
        self.service.symbol_references()
    }

    /// The issues of `file_id` and what its analysis found in it, as the type of each expression, against the last
    /// analysis's codebase. `None` when the file is not in the database.
    #[must_use]
    pub fn analyze_file_with_artifacts(&self, file_id: FileId) -> Option<(IssueCollection, AnalysisArtifacts)> {
        self.service.analyze_file_with_artifacts(file_id)
    }

    /// Mutably borrow the workspace file database (to add / update / delete
    /// files as the workspace changes).
    pub fn database_mut(&mut self) -> &mut Database<'static> {
        &mut self.database
    }

    /// Ends a run that exits next: drops what holds processes and files open, and leaks the files and the analysis,
    /// which the process's exit frees at once, where dropping them would walk every file, symbol and type on one
    /// thread.
    pub fn leak(self) {
        std::mem::forget(self.database);
        self.service.leak();
    }

    /// Run a full analysis pass over the workspace.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the underlying analysis fails. The failed
    /// pass leaves no state behind: the next pass analyzes the whole workspace.
    pub fn analyze(&mut self) -> Result<AnalysisResult, ServerError> {
        self.pass(IncrementalAnalysisService::analyze)
    }

    /// Refresh the analysis service's view of the database, then re-analyze the
    /// `changed` files, or the whole workspace when no pass has succeeded yet.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the underlying analysis fails. The failed
    /// pass leaves no state behind: the next pass analyzes the whole workspace.
    pub fn analyze_incremental(&mut self, changed: &[FileId]) -> Result<AnalysisResult, ServerError> {
        self.service.update_database(self.database.read_only());
        if self.service.is_initialized() {
            self.pass(|service| service.analyze_incremental(Some(changed)))
        } else {
            self.pass(IncrementalAnalysisService::analyze)
        }
    }

    /// Analyze with the extension hosts of `plugin_registry`, which register return-type providers and nothing else,
    /// from the last analysis, which ran without them and recorded `requests`, as
    /// [`IncrementalAnalysisService::analyze_with_hosts`] does: only the files whose requests a provider answers are
    /// analyzed again.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] if the analysis fails. The failed pass leaves no state behind.
    pub fn analyze_with_hosts(
        &mut self,
        plugin_registry: Arc<PluginRegistry>,
        requests: &ReturnTypeRequests,
    ) -> Result<AnalysisResult, ServerError> {
        self.pass(|service| service.analyze_with_hosts(plugin_registry, requests))
    }

    /// Take in that the compiled files of `classes`, each a lowercase class name, changed: start again each extension
    /// worker that loaded one, and give up what the last pass found in each file whose extension callbacks read one.
    /// Returns those files, for the next [`Self::analyze_incremental`] to analyze again, or `None` when only a pass
    /// over the whole workspace is exact.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when a worker cannot answer whether it loaded one, or cannot start again.
    pub fn reload_classes(
        &mut self,
        classes: impl IntoIterator<Item = Word>,
    ) -> Result<Option<Vec<FileId>>, ServerError> {
        Ok(self.service.reload_classes(&classes.into_iter().collect())?)
    }

    /// Compile each `.sharp` file of the last analysis into the bytes of its `.sharpc` file in `folder`, which the
    /// folder's `write` takes as soon as they are made, or the errors that refuse it there, and give the
    /// [`CompiledFiles`] the next compile into it keeps files by. A file whose key
    /// the folder's `last` holds keeps its compiled file, which the folder's `compiled` gives, and is not lowered again.
    /// One the folder's `intact` vouches for, as the compiled file the last compile wrote, is read only when a stamp it
    /// holds changed, and is [`Compilation::Kept`] otherwise. `hosts`, when given, is the folder the extension hosts
    /// load the project's classes from, where no error and no law refuses a file, compiled from the same lowerings.
    /// `stamp` gives the size, modification time and hash of the file at a workspace-relative path, or none when no
    /// file is there, once for each path, called from several threads at once. `check` gets the Lean translation of
    /// each file whose laws are checked again and of each file they reach, with the reach and each file the analysis
    /// reported an error in, and returns the issues of each file whose law is not proved. Each compiled file's header names `standard_library`, the standard library
    /// the workspace compiles against. `refusing` keeps the analysis's errors that refuse their file.
    ///
    /// [`CompiledFiles`]: mago_orchestrator::service::incremental_analysis::compile::CompiledFiles
    /// [`Compilation::Kept`]: mago_orchestrator::service::incremental_analysis::compile::Compilation::Kept
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when no analysis ran yet, or when re-analyzing a file, `compiled`, `stamp` or `check`
    /// fails.
    pub fn compile<'folder, Written>(
        &mut self,
        folder: Folder<'folder, Written>,
        hosts: Option<Folder<'folder, Written>>,
        stamp: impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync,
        check: impl FnOnce(Vec<Translation>, &Reach, &HashSet<FileId>) -> std::io::Result<HashMap<FileId, IssueCollection>>,
        standard_library: &Library,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> Result<(Compiled<Written>, Option<Compiled<Written>>), ServerError>
    where
        Written: Send,
    {
        self.service.compile(folder, hosts, stamp, check, standard_library, refusing).map_err(ServerError::from)
    }

    /// The issues that refuse the laws of the last analysis before any proof, as `mago compile` reports them, in the
    /// files the scope holds.
    ///
    /// No Lean process starts, so `mago analyze` reports every refusal of a law but its proof.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when no analysis ran yet, or when re-analyzing a file fails.
    pub fn law_refusals(&self) -> Result<IssueCollection, ServerError> {
        let in_scope =
            |file_id: &FileId| self.scope.as_ref().is_none_or(|scope| self.database.get_ref(file_id).is_ok_and(scope));

        Ok(self
            .service
            .law_refusals()?
            .into_iter()
            .filter(|(file_id, _)| in_scope(file_id))
            .flat_map(|(_, issues)| issues)
            .collect())
    }

    /// The standard library's inline forms, which `mago convert` reads to find the library member that runs a PHP
    /// function: those of the library files [`Self::compile`] lowers and doesn't refuse, by `refusing` as `compile`
    /// takes it.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError`] when no analysis ran yet, or when re-analyzing a library file fails.
    pub fn standard_library_forms(
        &mut self,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> Result<InlineForms, ServerError> {
        self.service.standard_library_forms(refusing).map_err(ServerError::from)
    }

    /// Every PHP# listener of the last analysis, in the order the engine runs them.
    #[must_use]
    pub fn listeners(&self) -> Vec<Listener> {
        self.service.listeners()
    }

    /// Every PHP# family of the last analysis, by its full name, with each variant's name and class.
    #[must_use]
    pub fn families(&self) -> Vec<Family> {
        self.service.families()
    }

    /// The app's module whose binding of the dispatcher serves the whole app.
    #[must_use]
    pub fn dispatcher_module(&self) -> Option<DispatcherModule> {
        self.service.dispatcher_module()
    }

    /// Every `extern` line of the last analysis that gives a class, a method or a function attributes, by what it
    /// declares, then by the file.
    #[must_use]
    pub fn extern_attributes(&self) -> Vec<ExternAttributes> {
        self.service.extern_attributes()
    }

    /// The effects of each plain PHP class, method and function an `extern` line of the last analysis declares, by
    /// what it declares.
    #[must_use]
    pub fn extern_effects(&self) -> Vec<ExternEffects> {
        self.service.extern_effects()
    }

    /// The effects the last analysis solves over every file's bodies, which give a PHP# method the effects a function
    /// type of it lists, spec section 29, as `mago convert` reads them for the invoker it writes.
    #[must_use]
    pub fn effects(&self) -> Effects {
        self.service.effects()
    }

    /// For each of `files`, the source files other than it whose signature edit makes the last analysis's warm path
    /// re-analyze it, which `mago convert` reads to find the converted files an issue in another file can come from.
    #[must_use]
    pub fn files_reaching(&self, files: &[FileId]) -> Vec<HashSet<FileId>> {
        self.service.files_reaching(files)
    }

    /// Run one analysis pass with the node-analysis hooks only in the scope's files, and report
    /// only their issues. A failed pass resets the service. The database then holds each file the
    /// autoloader served the pass, so an issue's annotation in one renders.
    fn pass(
        &mut self,
        analyze: impl FnOnce(&mut IncrementalAnalysisService) -> Result<AnalysisResult, OrchestratorError>,
    ) -> Result<AnalysisResult, ServerError> {
        let files = self
            .scope
            .as_ref()
            .map(|scope| self.database.files().filter(|file| scope(file)).map(|file| file.id).collect::<HashSet<_>>());
        self.service.set_node_analysis_files(files.clone());

        let result = analyze(&mut self.service)
            .inspect_err(|_| self.service.reset())
            .map(|result| reported_in(result, files.as_ref()))
            .map_err(ServerError::from)?;
        for file in self.service.autoloaded() {
            if self.database.get(&file.id).is_err() {
                self.database.add(Arc::clone(file));
            }
        }

        Ok(result)
    }
}

/// `result` with only the issues reported in `files`, plus the issues that name no file, or every
/// issue when `files` is `None`.
fn reported_in(mut result: AnalysisResult, files: Option<&HashSet<FileId>>) -> AnalysisResult {
    if let Some(files) = files {
        result.issues = std::mem::take(&mut result.issues)
            .into_iter()
            .filter(|issue| reported_file(issue).is_none_or(|file_id| files.contains(&file_id)))
            .collect();
    }

    result
}

/// The file `issue` is reported in: its primary annotation's file, else its first annotation's, or
/// `None` when it names no file.
fn reported_file(issue: &Issue) -> Option<FileId> {
    issue
        .annotations
        .iter()
        .find(|annotation| annotation.kind.is_primary())
        .or_else(|| issue.annotations.first())
        .map(|annotation| annotation.span.file_id)
        .filter(|file_id| !file_id.is_zero())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;

    use mago_analyzer::plugin::PluginRegistry;
    use mago_analyzer::settings::Settings as AnalyzerSettings;
    use mago_database::DatabaseConfiguration;
    use mago_database::file::FileType;
    use mago_reporting::Annotation;
    use mago_syntax::settings::ParserSettings;

    use super::*;

    fn host(name: &'static str, contents: &'static str) -> File {
        File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            Some(Path::new("/scope").join(name)),
            Cow::Borrowed(contents.as_bytes()),
        )
    }

    /// A server over two host files, `src/a.php` and `src/b.php`, that each report an issue.
    fn two_file_server() -> (Server, FileId, FileId) {
        let mut database =
            Database::new(DatabaseConfiguration::new(Path::new("/scope"), vec![], vec![], vec![], vec![]));
        let a = database.add(host("src/a.php", "<?php\nfunction a(): int { return 'a'; }\n"));
        let b = database.add(host("src/b.php", "<?php\nfunction b(): int { return 'b'; }\n"));
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
            autoloader: None,
        };

        (Server::new(database, Default::default, settings), a, b)
    }

    #[test]
    fn a_scoped_server_reports_the_issues_whose_primary_annotation_lies_in_its_scope() {
        let (mut server, _, b) = two_file_server();
        let all = server.analyze().expect("analysis").issues;

        let scoped = two_file_server()
            .0
            .scoped_to(|file| file.name.as_ref() == b"src/b.php")
            .analyze()
            .expect("scoped analysis")
            .issues;

        let primary_file = |issue: &Issue| {
            issue
                .annotations
                .iter()
                .find(|annotation| annotation.kind.is_primary())
                .map(|annotation| annotation.span.file_id)
        };
        assert!(all.iter().any(|issue| primary_file(issue).is_some_and(|file| file != b)), "{all:#?}");
        assert!(!scoped.is_empty());
        assert_eq!(
            scoped.iter().collect::<Vec<_>>(),
            all.iter().filter(|issue| primary_file(issue) == Some(b)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_issue_is_reported_in_its_primary_file_else_its_first_annotated_file() {
        let (mut server, a, b) = two_file_server();
        let issues = server.analyze().expect("analysis").issues;
        let span_in = |file_id: FileId| {
            issues
                .iter()
                .flat_map(|issue| &issue.annotations)
                .find(|annotation| annotation.span.file_id == file_id)
                .expect("an issue in the file")
                .span
        };

        let primary =
            Issue::error("x").with_annotations([Annotation::secondary(span_in(a)), Annotation::primary(span_in(b))]);
        let secondary_only =
            Issue::error("x").with_annotations([Annotation::secondary(span_in(a)), Annotation::secondary(span_in(b))]);

        assert_eq!(reported_file(&primary), Some(b));
        assert_eq!(reported_file(&secondary_only), Some(a));
        assert_eq!(reported_file(&Issue::error("x")), None);
    }
}
