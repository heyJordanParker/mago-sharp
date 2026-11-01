#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::missing_panics_doc, clippy::panic, clippy::unwrap_used)]

use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use mago_analyzer::external::AFTER_FILE_ANALYSIS_BATCH_SIZE;
use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::GlobSettings;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_formatter::settings::FormatSettings;
use mago_guard::settings::Settings as GuardSettings;
use mago_linter::settings::Settings as LinterSettings;
use mago_orchestrator::Orchestrator;
use mago_orchestrator::OrchestratorConfiguration;
use mago_orchestrator::service::analysis::AnalysisService;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_reporting::IssueCollection;
use mago_syntax::settings::ParserSettings;
use mago_text_edit::Safety;
use serde::Deserialize;

mod common;

const FILE_COUNT: usize = AFTER_FILE_ANALYSIS_BATCH_SIZE + 1;
const NODE_TARGET_COUNT: usize = 12;
const PLUGINS: [&str; 2] = ["lifecycle-one", "lifecycle-two"];

#[derive(Debug, Deserialize)]
struct AuditEntry(String, String, Option<String>, u32);

fn make_database(
    changed_file: Option<usize>,
    framework_reference_enabled: bool,
    late_reference_enabled: bool,
    external_sources: &[(Vec<u8>, Vec<u8>)],
) -> (Database<'static>, FileId) {
    let configuration = common::database_configuration("/lifecycle-proof", vec![Cow::Borrowed(b"src")]);
    let mut database = Database::new(configuration);
    let mut changed_id = FileId::zero();
    for index in 0..FILE_COUNT {
        let value = if changed_file == Some(index) { index + 100 } else { index };
        let generator = if index == FILE_COUNT - 1 {
            format!("\nfunction lifecycle_generator(): iterable {{ yield {index} => 'value'; }}\n")
        } else {
            String::new()
        };
        let extension_usage = if index == 0 {
            r#"
/**
 * @assert int $value
 * @assert-if-true non-empty-string $text
 * @assert-if-false null $fallback
 */
function lifecycle_assertions(mixed $value, mixed $text, mixed $fallback): bool { return true; }

function extension_consumer(
    LifecycleClass0 $provided,
    bool $condition,
    mixed $definedBeforeLoop,
): int {
    if ($condition) {
        $possiblyDefinedBeforeLoop = null;
    }

    foreach ([1] as $definedBeforeLoop) {}
    for ($undefinedBeforeLoop = 0; $undefinedBeforeLoop < 1; ++$undefinedBeforeLoop) {}

    return $provided->answer() + $provided->unrelated() + extension_answer(0) + EXTENSION_ANSWER;
}

/** @mago-expect analysis:non-existent-class-like */
final class IncompleteLifecycleClass implements Vendor\Unresolvable\LifecycleContract {}

$lifecycleInstance = new LifecycleClass0();
$lifecycleInstance->value();
$lifecycleInstance->topLevelProperty = $lifecycleInstance->topLevelProperty + 1;
$lifecycleInstance->closureSource();
lifecycle_function_0(1);

$lifecycleClosure = function () use ($lifecycleInstance): int { return $lifecycleInstance->fileClosureTarget(); };
$lifecycleArrow = fn(): int => $lifecycleInstance->fileArrowTarget();
"#
        } else {
            ""
        };
        let inheritance = if index == 0 { " extends ExtensionProvided" } else { "" };
        let private_methods = if index == 0 {
            "\n    public int $topLevelProperty = 0;\n\n    public function closureSource(): void { (function (): void { $this->closureTarget(); })(); }\n\n    public function fileClosureTarget(): int { return 1; }\n\n    public function fileArrowTarget(): int { return 1; }\n\n    private function closureTarget(): void {}\n\n    private function frameworkAction(): void {}\n\n    private function lateFrameworkAction(): void {}\n\n    private function actuallyUnused(): void {}\n\n    /**\n     * @param non-empty-lowercase-string|'' $first\n     * @param non-empty-lowercase-string|'' $second\n     * @return list<string>\n     */\n    public function lowercasePair(string $first, string $second): array { return [$first, $second]; }\n"
        } else {
            ""
        };
        let framework_reference_marker =
            if index == 0 && framework_reference_enabled { "\nconst ENABLE_FRAMEWORK_ACTION = true;\n" } else { "" };
        let late_reference_marker =
            if index == 5 && late_reference_enabled { "\nconst ENABLE_LATE_FRAMEWORK_ACTION = true;\n" } else { "" };
        let after_file_expectation =
            if index == 0 { "// @mago-expect analysis:lifecycle-one/after-file\n" } else { "" };
        let after_analysis_expectation = if index == 0 { "// @mago-expect analysis:lifecycle-one/after\n" } else { "" };
        let contents = format!(
            "<?php\n\n/* exact-in-memory-source:{index}:{value} */\n\ndeclare(strict_types=1);\n\n{after_file_expectation}{after_analysis_expectation}final class LifecycleClass{index}{inheritance} {{\n    public function value(): int {{ return {value}; }}\n{private_methods}}}\n\nfunction lifecycle_function_{index}(int $value): int {{ return $value + {value}; }}\n{extension_usage}{framework_reference_marker}{late_reference_marker}{generator}"
        );
        let file = File::new(
            Cow::Owned(format!("src/file{index}.php").into_bytes()),
            FileType::Host,
            None,
            Cow::Owned(contents.into_bytes()),
        );
        if index == changed_file.unwrap_or_default() {
            changed_id = file.id;
        }
        database.add(file);
    }

    for (name, contents) in external_sources {
        database.add(File::new(Cow::Owned(name.clone()), FileType::External, None, Cow::Owned(contents.clone())));
    }

    (database, changed_id)
}

fn read_audit(path: &Path) -> Vec<AuditEntry> {
    std::fs::read_to_string(path)
        .expect("lifecycle audit log should be readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("lifecycle audit entry should be valid JSON"))
        .collect()
}

fn count_code(issues: &IssueCollection, code: &str) -> usize {
    issues.iter().filter(|issue| issue.code.as_deref() == Some(code)).count()
}

fn assert_issue_cardinality(issues: &IssueCollection, unused_methods: usize) {
    assert_eq!(count_code(issues, "unused-method"), unused_methods);
    assert_eq!(count_code(issues, "unfulfilled-expect"), 0);
    for plugin in PLUGINS {
        assert_eq!(count_code(issues, &format!("{plugin}/before")), 1);
        assert_eq!(
            count_code(issues, &format!("{plugin}/after-file")),
            FILE_COUNT - usize::from(plugin == "lifecycle-one")
        );
        assert_eq!(count_code(issues, &format!("{plugin}/after")), usize::from(plugin != "lifecycle-one"));
    }

    let final_issue = issues
        .iter()
        .find(|issue| issue.code.as_deref() == Some("lifecycle-two/after"))
        .expect("the final lifecycle issue should exist");
    assert_eq!(final_issue.annotations.len(), 2);
    assert_ne!(final_issue.annotations[0].span.file_id, final_issue.annotations[1].span.file_id);
    let final_edits = final_issue
        .edits
        .get(&final_issue.annotations[0].span.file_id)
        .expect("the project-wide edit should resolve its named source file");
    assert_eq!(final_edits.len(), 1);
    assert_eq!(final_edits[0].range.start, final_issue.annotations[0].span.start.offset);
    assert_eq!(final_edits[0].range.end, final_issue.annotations[0].span.end.offset);
    assert!(final_edits[0].new_text.is_empty());
    assert_eq!(final_edits[0].safety, Safety::Unsafe);

    for issue in issues.iter().filter(|issue| issue.code.as_deref().is_some_and(|code| code.ends_with("/after-file"))) {
        assert_eq!(issue.edits.len(), 1);
        let (file_id, edits) = issue.edits.iter().next().expect("an after-file issue should contain one edit batch");
        assert_eq!(*file_id, issue.annotations[0].span.file_id);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start, 0);
        assert_eq!(edits[0].range.end, 5);
        assert_eq!(edits[0].new_text, b"<?php");
        assert_eq!(edits[0].safety, Safety::PotentiallyUnsafe);
    }
}

fn assert_initial_audit(entries: &[AuditEntry]) {
    assert_eq!(entries.iter().filter(|entry| entry.1 == "initialize").count(), PLUGINS.len() * 3);
    assert_eq!(entries.iter().filter(|entry| entry.1 == "before").count(), PLUGINS.len());
    assert_eq!(entries.iter().filter(|entry| entry.1 == "after-file").count(), FILE_COUNT * PLUGINS.len());
    assert_eq!(entries.iter().filter(|entry| entry.1 == "node").count(), PLUGINS.len() * NODE_TARGET_COUNT);
    assert_eq!(entries.iter().filter(|entry| entry.1 == "method-call").count(), PLUGINS.len());
    assert_eq!(entries.iter().filter(|entry| entry.1 == "class-like").count(), PLUGINS.len());
    assert_eq!(entries.iter().filter(|entry| entry.1 == "after").count(), PLUGINS.len());

    let callbacks = entries
        .iter()
        .filter(|entry| entry.1 == "after-file")
        .map(|entry| (entry.0.as_str(), entry.2.as_deref()))
        .collect::<HashSet<_>>();
    assert_eq!(callbacks.len(), FILE_COUNT * PLUGINS.len());

    let workers = entries.iter().filter(|entry| entry.1 == "after-file").map(|entry| entry.3).collect::<HashSet<_>>();
    assert!(workers.len() > 1, "per-file hooks should use more than one worker");
    for plugin in PLUGINS {
        let initialization_workers = entries
            .iter()
            .filter(|entry| entry.0 == plugin && entry.1 == "initialize")
            .map(|entry| entry.3)
            .collect::<HashSet<_>>();
        assert_eq!(initialization_workers.len(), 3, "initialization must run on every worker");
    }
}

#[test]
fn external_analyzer_lifecycle_is_exact_across_workers_and_incremental_runs() {
    if std::env::var_os("MAGO_TRACE_PHP_SDK_TESTS").is_some() {
        let _subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external analyzer lifecycle test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary lifecycle directory should be created");
    let audit_log = temporary.path().join("audit.jsonl");
    std::fs::write(&audit_log, []).expect("lifecycle audit log should be initialized");

    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-lifecycle-worker.php"))
        .with_current_directory(repository)
        .with_environment("MAGO_LIFECYCLE_AUDIT_LOG", &audit_log);
    let pool =
        WorkerPool::spawn(command, NonZeroUsize::new(3).expect("three is non-zero"), WorkerPoolOptions::default())
            .expect("PHP lifecycle worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("PHP lifecycle analyzer should initialize");
    let initialization_sources = analyzer
        .initialization_files()
        .into_iter()
        .map(|file| (file.name.into_owned(), file.contents.into_owned()))
        .collect::<Vec<_>>();
    assert_eq!(initialization_sources.len(), 1);
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    let (database, _) = make_database(None, true, true, &initialization_sources);
    let mut settings = Settings::new(PHPVersion::PHP85);
    settings.find_unused_expressions = false;
    settings.find_unused_definitions = true;
    let mut service = IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        settings,
        ParserSettings::default(),
        Arc::new(registry),
    );

    let initial = service.analyze().expect("initial lifecycle analysis should succeed");
    let unexpected = initial
        .issues
        .iter()
        .filter(|issue| {
            issue.code.as_deref().is_none_or(|code| !code.starts_with("lifecycle-") && code != "unused-method")
        })
        .map(|issue| (issue.code.as_deref(), issue.message.as_str()))
        .collect::<Vec<_>>();
    assert!(unexpected.is_empty(), "unexpected analyzer issues: {unexpected:#?}");
    assert_eq!(count_code(&initial.issues, "unused-method"), 1);
    assert_issue_cardinality(&initial.issues, 1);
    let external_annotation = initial
        .issues
        .iter()
        .find(|issue| issue.code.as_deref() == Some("lifecycle-two/after"))
        .expect("the final lifecycle issue should exist")
        .annotations[1]
        .span;
    assert!(
        service.database().get_ref(&external_annotation.file_id).is_ok_and(|file| file.file_type.is_external()),
        "reporting must be able to resolve annotations into external stubs"
    );

    let initial_audit = read_audit(&audit_log);
    assert_eq!(initial_audit.len(), PLUGINS.len() * (7 + NODE_TARGET_COUNT + FILE_COUNT));
    assert_initial_audit(&initial_audit);

    let (updated_database, changed_file) = make_database(Some(5), true, true, &initialization_sources);
    service.update_database(updated_database.read_only());
    let incremental =
        service.analyze_incremental(Some(&[changed_file])).expect("incremental lifecycle analysis should succeed");
    assert_issue_cardinality(&incremental.issues, 1);

    let audit = read_audit(&audit_log);
    let incremental_audit = &audit[initial_audit.len()..];
    assert_eq!(incremental_audit.iter().filter(|entry| entry.1 == "before").count(), PLUGINS.len());
    assert_eq!(incremental_audit.iter().filter(|entry| entry.1 == "after-file").count(), PLUGINS.len());
    assert_eq!(incremental_audit.iter().filter(|entry| entry.1 == "node").count(), 0);
    assert_eq!(incremental_audit.iter().filter(|entry| entry.1 == "class-like").count(), 0);
    assert_eq!(incremental_audit.iter().filter(|entry| entry.1 == "after").count(), PLUGINS.len());
    assert!(
        incremental_audit
            .iter()
            .filter(|entry| entry.1 == "after-file")
            .all(|entry| entry.2.as_deref() == Some("src/file5.php"))
    );

    let first_changed_file = changed_file;
    let (second_updated_database, second_changed_file) = make_database(Some(6), true, true, &initialization_sources);
    service.update_database(second_updated_database.read_only());
    let second_incremental = service
        .analyze_incremental(Some(&[first_changed_file, second_changed_file]))
        .expect("a second incremental lifecycle analysis should rebuild extension metadata from source state");
    assert_issue_cardinality(&second_incremental.issues, 1);

    let second_audit = read_audit(&audit_log);
    let second_incremental_audit = &second_audit[audit.len()..];
    assert_eq!(second_incremental_audit.iter().filter(|entry| entry.1 == "before").count(), PLUGINS.len());
    assert_eq!(second_incremental_audit.iter().filter(|entry| entry.1 == "after-file").count(), PLUGINS.len() * 2);
    assert_eq!(second_incremental_audit.iter().filter(|entry| entry.1 == "after").count(), PLUGINS.len());
    let analyzed_files = second_incremental_audit
        .iter()
        .filter(|entry| entry.1 == "after-file")
        .filter_map(|entry| entry.2.as_deref())
        .collect::<HashSet<_>>();
    assert_eq!(analyzed_files, HashSet::from(["src/file5.php", "src/file6.php"]));

    let (without_late_reference, _) = make_database(Some(6), true, false, &initialization_sources);
    service.update_database(without_late_reference.read_only());
    let without_late_reference = service
        .analyze_incremental(Some(&[FileId::new(b"src/file5.php")]))
        .expect("removing a late reference should restore cached unused-member issues");
    assert_issue_cardinality(&without_late_reference.issues, 2);

    let without_late_reference_audit = read_audit(&audit_log);
    let late_reference_removal_audit = &without_late_reference_audit[second_audit.len()..];
    // Only `src/file5.php`'s hook reads `ENABLE_LATE_FRAMEWORK_ACTION`, but a hook may answer any file from what it
    // kept, so every file counts the read as its own and runs again when the constant leaves.
    assert_eq!(late_reference_removal_audit.iter().filter(|entry| entry.1 == "after-file").count(), FILE_COUNT * 2);

    service.update_database(second_updated_database.read_only());
    let restored_late_reference = service
        .analyze_incremental(Some(&[FileId::new(b"src/file5.php")]))
        .expect("restoring a late reference should suppress cached unused-member issues");
    assert_issue_cardinality(&restored_late_reference.issues, 1);

    let restored_late_reference_audit = read_audit(&audit_log);
    let late_reference_restoration_audit = &restored_late_reference_audit[without_late_reference_audit.len()..];
    assert_eq!(late_reference_restoration_audit.iter().filter(|entry| entry.1 == "after-file").count(), FILE_COUNT * 2);

    let (without_framework_reference, _) = make_database(Some(6), false, true, &initialization_sources);
    service.update_database(without_framework_reference.read_only());
    let without_reference = service
        .analyze_incremental(Some(&[FileId::new(b"src/file0.php")]))
        .expect("removing an external reference should invalidate cached unused-member issues");
    assert_issue_cardinality(&without_reference.issues, 2);

    let without_reference_audit = read_audit(&audit_log);
    let reference_removal_audit = &without_reference_audit[restored_late_reference_audit.len()..];
    assert_eq!(reference_removal_audit.iter().filter(|entry| entry.1 == "after-file").count(), FILE_COUNT * 2);

    service.update_database(second_updated_database.read_only());
    let restored_reference = service
        .analyze_incremental(Some(&[FileId::new(b"src/file0.php")]))
        .expect("restoring an external reference should invalidate cached unused-member issues");
    assert_issue_cardinality(&restored_reference.issues, 1);

    let restored_reference_audit = read_audit(&audit_log);
    let reference_restoration_audit = &restored_reference_audit[without_reference_audit.len()..];
    assert_eq!(reference_restoration_audit.iter().filter(|entry| entry.1 == "after-file").count(), FILE_COUNT * 2);
}

#[test]
fn orchestrator_loads_initialization_stubs_into_the_source_database() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external analyzer initialization-stub test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary lifecycle directory should be created");
    let audit_log = temporary.path().join("audit.jsonl");
    std::fs::write(&audit_log, []).expect("lifecycle audit log should be initialized");
    std::fs::write(temporary.path().join("host.php"), "<?php\nfinal class HostClass {}\n")
        .expect("host source should be created");

    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-lifecycle-worker.php"))
        .with_current_directory(repository)
        .with_environment("MAGO_LIFECYCLE_AUDIT_LOG", &audit_log);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
        .expect("PHP lifecycle worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("PHP lifecycle analyzer should initialize");
    let orchestrator = Orchestrator::new(OrchestratorConfiguration {
        php_version: PHPVersion::PHP85,
        paths: vec![],
        includes: vec![],
        patches: vec![],
        excludes: vec![],
        extensions: vec!["php"],
        glob: GlobSettings::default(),
        parser_settings: ParserSettings::default(),
        analyzer_settings: Settings::new(PHPVersion::PHP85),
        linter_settings: LinterSettings::default(),
        guard_settings: GuardSettings::default(),
        formatter_settings: FormatSettings::default(),
        disable_default_analyzer_plugins: false,
        analyzer_plugins: vec![],
        use_progress_bars: false,
        use_colors: false,
    });

    orchestrator.set_external_analyzer(analyzer);

    let database = orchestrator
        .load_database(temporary.path(), true, None, None)
        .expect("database loading should include initialization stubs");
    let external = database.files().filter(|file| file.file_type.is_external()).collect::<Vec<_>>();
    assert_eq!(external.len(), 1);
    assert!(external[0].path.is_none());
    assert!(external[0].name.starts_with(b"@mago-extension/"));
    assert!(
        external[0]
            .contents
            .windows(b"class ExtensionProvided".len())
            .any(|bytes| { bytes == b"class ExtensionProvided" })
    );
}

/// The `request-timeout-ms` of each host the deadline tests start.
const DEADLINE: Duration = Duration::from_secs(3);

/// An extension host whose plugin `slow-routes` has an after-analysis hook that runs for 0.6 times `DEADLINE`, and
/// whose plugin `dead-routes` has one that never finishes.
const HOST_WHOSE_SECOND_AFTER_ANALYSIS_HOOK_NEVER_FINISHES: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class Routes implements Plugin, AfterAnalysisHook
{
    public function __construct(
        private readonly string $identifier,
        private readonly ?int $microseconds,
    ) {}

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition($this->identifier, $this->identifier, 'Reports the routes no request reaches.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterAnalysisHook($this);
    }

    public function afterAnalysis(AfterAnalysisContext $context): void
    {
        if ($this->microseconds !== null) {
            usleep($this->microseconds);

            return;
        }

        while (true) {
            sleep(1);
        }
    }
}

(new Worker(new Extension('acme/routes', 'Routes', '1.0.0', analyzerPlugins: [
    new Routes('slow-routes', 1_800_000),
    new Routes('dead-routes', null),
])))->run();
"#;

/// An extension host whose plugin `hung-filter` has an issue-filter hook for `non-existent-function` that never
/// finishes.
const HOST_WHOSE_ISSUE_FILTER_NEVER_FINISHES: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\IssueFilterContext;
use Mago\Sdk\Analyzer\IssueFilterDecision;
use Mago\Sdk\Analyzer\IssueFilterHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class HungFilter implements Plugin, IssueFilterHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('hung-filter', 'Hung filter', 'Never decides.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerIssueFilterHook($this);
    }

    public function getCodes(): array
    {
        return ['non-existent-function'];
    }

    public function filterIssue(IssueFilterContext $context): IssueFilterDecision
    {
        while (true) {
            sleep(1);
        }
    }
}

(new Worker(new Extension('acme/filter', 'Filter', '1.0.0', analyzerPlugins: [new HungFilter()])))->run();
"#;

/// An extension host whose plugin `hung-file` has an after-file hook that never finishes.
const HOST_WHOSE_AFTER_FILE_HOOK_NEVER_FINISHES: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class HungFile implements Plugin, AfterFileAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('hung-file', 'Hung file', 'Never finishes a file.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterFileAnalysisHook($this);
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function afterFileAnalysis(AfterFileAnalysisContext $context): void
    {
        while (true) {
            sleep(1);
        }
    }
}

(new Worker(new Extension('acme/file', 'File', '1.0.0', analyzerPlugins: [new HungFile()])))->run();
"#;

/// The plugin registry of an extension host `acme` of one worker that runs `source`, a PHP worker script written into
/// `directory`, with `DEADLINE` as its `request-timeout-ms`.
fn deadline_registry(source: &str, directory: &Path) -> PluginRegistry {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let host = directory.join("host.php");
    std::fs::write(&host, source).expect("host source should be written");
    let command = WorkerCommand::new("php").with_argument(host).with_argument(repository.join("vendor/autoload.php"));
    let options =
        WorkerPoolOptions { host: "acme".to_owned(), request_timeout: DEADLINE, ..WorkerPoolOptions::default() };
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, options).expect("PHP host pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("PHP host analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));
    registry
}

/// A project of one file, `src/Route.php`, which calls a function that does not exist.
fn deadline_database() -> Database<'static> {
    let mut database = Database::new(common::database_configuration("/deadline-proof", vec![Cow::Borrowed(b"src")]));
    database.add(File::new(
        Cow::Borrowed(b"src/Route.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\n\nfinal class Route {}\n\nmissing_route();\n"),
    ));
    database
}

/// The host runs every plugin's after-analysis hooks in one request, and starts the deadline again at each plugin's,
/// so a plugin's hooks that finish in time leave the next plugin its whole deadline. The error names the plugin whose
/// hooks made no progress, as it names the file whose hooks made none.
#[test]
fn an_after_analysis_hook_that_never_finishes_fails_at_the_deadline_naming_its_plugin() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the after-analysis deadline test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary host directory should be created");
    let registry = deadline_registry(HOST_WHOSE_SECOND_AFTER_ANALYSIS_HOOK_NEVER_FINISHES, temporary.path());
    let database = deadline_database();
    let mut service = IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        Settings::new(PHPVersion::PHP85),
        ParserSettings::default(),
        Arc::new(registry),
    );

    let started_at = Instant::now();
    let error = service.analyze().expect_err("the after-analysis hook should fail at the deadline");
    let elapsed = started_at.elapsed();

    // `slow-routes` runs for 0.6 times the deadline, then `dead-routes` hangs for the whole deadline.
    assert!(elapsed > DEADLINE * 6 / 5 && elapsed < DEADLINE * 13 / 5, "the analysis failed after {elapsed:?}");
    assert_eq!(
        error.to_string(),
        "Analysis error: Plugin Error: External plugin error: external analyzer worker failed: extension host `acme` made no progress for 3000 ms on the after-analysis hooks of plugin `dead-routes` (worker 0): raise its `request-timeout-ms` for hooks that run that long"
    );
}

/// An issue-filter request runs one file's issue-filter hooks, so Mago names it by the file, and the worker sends no
/// progress for it.
#[test]
fn an_issue_filter_that_never_finishes_fails_at_the_deadline_naming_its_file() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the issue-filter deadline test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary host directory should be created");
    let registry = deadline_registry(HOST_WHOSE_ISSUE_FILTER_NEVER_FINISHES, temporary.path());
    let database = deadline_database();
    let mut service = IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        Settings::new(PHPVersion::PHP85),
        ParserSettings::default(),
        Arc::new(registry),
    );

    let started_at = Instant::now();
    let error = service.analyze().expect_err("the issue filter should fail at the deadline");
    let elapsed = started_at.elapsed();

    assert!(elapsed >= DEADLINE && elapsed < DEADLINE * 2, "the analysis failed after {elapsed:?}");
    assert_eq!(
        error.to_string(),
        "Analysis error: Plugin Error: External plugin error: external analyzer worker failed: extension host `acme` made no progress for 3000 ms on the issue-filter hooks of src/Route.php (worker 0): raise its `request-timeout-ms` for hooks that run that long"
    );
}

/// An after-file request runs one file's hooks, so Mago names it by the file, and the worker sends no progress for
/// it.
#[test]
fn an_after_file_hook_that_never_finishes_fails_at_the_deadline_naming_its_file() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the after-file deadline test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary host directory should be created");
    let registry = deadline_registry(HOST_WHOSE_AFTER_FILE_HOOK_NEVER_FINISHES, temporary.path());
    let database = deadline_database();
    let service = AnalysisService::new(
        database.read_only(),
        CodebaseMetadata::new(),
        SymbolReferences::new(),
        Settings::new(PHPVersion::PHP85),
        ParserSettings::default(),
        Arc::new(registry),
    );

    let started_at = Instant::now();
    let error = service.oneshot(FileId::new(b"src/Route.php")).expect_err("the after-file hook should fail");
    let elapsed = started_at.elapsed();

    assert!(elapsed >= DEADLINE && elapsed < DEADLINE * 2, "the analysis failed after {elapsed:?}");
    assert_eq!(
        error.to_string(),
        "Analysis error: Plugin Error: External plugin error: external analyzer worker failed: extension host `acme` made no progress for 3000 ms on the hooks of src/Route.php (worker 0): raise its `request-timeout-ms` for hooks that run that long"
    );
}
