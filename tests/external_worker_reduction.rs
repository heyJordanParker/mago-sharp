#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_linter::external::ExternalLinter;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_syntax::settings::ParserSettings;

use crate::common;

/// One rule that reports a method body written in two files, in two extensions of one host: `acme/reduced` collects
/// each file's bodies in an after-file hook on whichever worker analyzes it and reports from its worker reducer, and
/// `acme/walked` walks every file in an after-analysis hook.
const DUPLICATE_BODIES_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Syntax\SourceFile;
use Mago\Sdk\Worker;
use Mago\Sdk\WorkerReducer;
use Mago\Sdk\WorkerReductionContext;

/** Each method body of `$source`, as [its text, its file, its start, its end]. */
function method_bodies(SourceFile $source): array
{
    $bodies = [];
    foreach ($source->getNodes(NodeKind::Method) as $method) {
        $body = $source->getFirstDescendant($method, NodeKind::MethodBody);
        if ($body !== null) {
            $bodies[] = [$source->getText($body), $source->path, $body->span->start, $body->span->end];
        }
    }

    return $bodies;
}

/** Each body of `$bodies` whose text an earlier one in file order has, with the file of that earlier one. */
function repeated_bodies(array $bodies): array
{
    usort($bodies, static fn(array $a, array $b): int => [$a[1], $a[2]] <=> [$b[1], $b[2]]);
    $first = [];
    $repeated = [];
    foreach ($bodies as $body) {
        if (isset($first[$body[0]])) {
            $repeated[] = [$body, $first[$body[0]]];
        } else {
            $first[$body[0]] = $body[1];
        }
    }

    return $repeated;
}

function repeat_issue(array $body, string $first): Issue
{
    return Issue::at("This body repeats the one in {$first}.", new SourceLocation($body[1], new Span($body[2], $body[3])));
}

final class ReducedBodies implements Plugin, AfterFileAnalysisHook, WorkerReducer
{
    private array $bodies = [];

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('reduced-bodies', 'Reduced bodies', 'Reports a method body written twice from its reducer.');
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
        array_push($this->bodies, ...method_bodies($context->analysis->getSourceFile()));
    }

    public function collect(): string
    {
        $collected = serialize($this->bodies);
        $this->bodies = [];

        return $collected;
    }

    public function reduce(WorkerReductionContext $context): void
    {
        $bodies = array_merge(...array_map('unserialize', $context->workerPayloads));
        foreach (repeated_bodies($bodies) as [$body, $first]) {
            $context->report(Level::Error, 'duplicate-body', repeat_issue($body, $first));
        }
    }
}

final class WalkedBodies implements Plugin, AfterAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('walked-bodies', 'Walked bodies', 'Reports a method body written twice from one walk.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterAnalysisHook($this);
    }

    public function afterAnalysis(AfterAnalysisContext $context): void
    {
        $bodies = [];
        foreach ($context->analysis->files as $file) {
            array_push($bodies, ...method_bodies($file->getSourceFile()));
        }
        foreach (repeated_bodies($bodies) as [$body, $first]) {
            $context->report(Level::Error, 'duplicate-body', repeat_issue($body, $first));
        }
    }
}

$reduced = new ReducedBodies();
(new Worker(
    new Extension('acme/reduced', 'Reduced', '1.0.0', analyzerPlugins: [$reduced], workerReducer: $reduced),
    new Extension('acme/walked', 'Walked', '1.0.0', analyzerPlugins: [new WalkedBodies()]),
))->run();
"#;

/// A worker reducer reports from the combined result of every worker, once per analysis, after every worker ran its
/// hooks on every file. `total()` has one body in `src/First.php` and `src/Second.php`, and another in
/// `src/Third.php`, and the analysis runs on three threads, so each worker holds some of the files. The reducer reports
/// the body once, at `src/Second.php`, as the after-analysis hook that walks every file on one thread reports it.
#[test]
fn a_worker_reducer_reports_a_body_written_in_two_files_once_at_the_second()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the worker-reducer report test") {
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let file = temporary.path().join("host.php");
    std::fs::write(&file, DUPLICATE_BODIES_HOST)?;
    let mut database = Database::new(common::database_configuration("/reduction-proof", vec![Cow::Borrowed(b"src")]));
    for (name, body) in [("First", "$this->price * 2"), ("Second", "$this->price * 2"), ("Third", "$this->price + 1")] {
        database.add(File::new(
            Cow::Owned(format!("src/{name}.php").into_bytes()),
            FileType::Host,
            None,
            Cow::Owned(
                format!(
                    "<?php\n\nfinal class {name}\n{{\n    public int $price = 1;\n\n    public function total(): int\n    {{\n        return {body};\n    }}\n}}\n"
                )
                .into_bytes(),
            ),
        ));
    }
    let second = database.get_by_name(b"src/Second.php")?.id;

    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let issues = pool.install(|| -> Result<Vec<Issue>, Box<dyn std::error::Error + Send + Sync>> {
        let analyzer = ExternalAnalyzer::initialize(
            [common::host(&file, common::NO_DEADLINE, common::NO_DEADLINE, &[])],
            PHPVersion::PHP85,
            &[],
            false,
        )?;
        let mut registry = PluginRegistry::with_library_providers();
        registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));
        let mut service = IncrementalAnalysisService::new(
            database.read_only(),
            Default::default,
            Settings::new(PHPVersion::PHP85),
            ParserSettings::default(),
            Arc::new(registry),
        );

        Ok(service.analyze()?.issues.into_iter().collect())
    })?;

    let reported = |code: &str| -> Vec<(String, Option<mago_span::Span>)> {
        issues
            .iter()
            .filter(|issue| issue.code.as_deref() == Some(code))
            .map(|issue| (issue.message.clone(), issue.primary_span()))
            .collect()
    };
    let reduced = reported("acme/reduced/duplicate-body");
    assert_eq!(reduced.len(), 1, "{issues:#?}");
    assert_eq!(reduced[0].0, "This body repeats the one in src/First.php.");
    assert_eq!(reduced[0].1.map(|span| span.file_id), Some(second));
    assert_eq!(reported("walked-bodies/duplicate-body"), reduced, "the walk reports what the reducer reports");

    Ok(())
}

/// Each interpreter that served the host is one worker. Once mago stops the host, its reducers collect each one's
/// state, on the thread it runs on, and reduce all of it in one of them. The host runs in a pool of its own, so no other
/// test's engine takes one of its threads meanwhile.
#[test]
fn worker_reducers_merge_every_interpreter_in_one() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("MAGO_TRACE_PHP_SDK_TESTS").is_some() {
        let _subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external worker-reduction test") {
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let host = common::host(
        &repository.join("composer/tests/Sdk/Fixtures/worker-reducer.php"),
        common::NO_DEADLINE,
        common::NO_DEADLINE,
        &[("MAGO_REDUCTION_AUDIT_DIRECTORY", temporary.path().to_str().expect("a UTF-8 path"))],
    );
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let started = pool.install(move || {
        let started = rayon::broadcast(|_| ExternalLinter::initialize([host.clone()], PHPVersion::PHP85).is_ok());
        drop(host);
        started
    });

    let alpha = read_audit(&temporary.path().join("alpha.txt"), "alpha")?;
    let beta = read_audit(&temporary.path().join("beta.txt"), "beta")?;
    assert!(started.iter().all(|started| *started));
    assert_eq!(alpha.worker_ids, beta.worker_ids, "logical extensions must observe the same worker order");
    assert_eq!(alpha.worker_ids.iter().collect::<HashSet<_>>().len(), 3);
    assert!(alpha.worker_ids.contains(&alpha.leader), "the reducing interpreter's own contribution must be included");
    assert!(beta.worker_ids.contains(&beta.leader), "the reducing interpreter's own contribution must be included");
    assert_eq!(alpha.leader, beta.leader, "all reducers must run in the same interpreter");

    Ok(())
}

struct Audit {
    leader: String,
    worker_ids: Vec<String>,
}

fn read_audit(path: &Path, identifier: &str) -> Result<Audit, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.first(), Some(&identifier));
    let worker_ids = lines[2..]
        .iter()
        .map(|payload| {
            payload
                .strip_prefix(identifier)
                .and_then(|payload| payload.strip_prefix(':'))
                .expect("payload must retain its extension identity")
                .to_string()
        })
        .collect();

    Ok(Audit { leader: lines[1].to_string(), worker_ids })
}
