<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\CodebaseScanContext;
use Mago\Sdk\Analyzer\CodebaseScanHook;
use Mago\Sdk\Analyzer\InitializationContext;
use Mago\Sdk\Analyzer\InitializationHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Exception\CancelledException;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Extension;
use Mago\Sdk\Internal\Analyzer\Protocol;
use Mago\Sdk\Internal\Io\ResourceReader;
use Mago\Sdk\Internal\Io\ResourceWriter;
use Mago\Sdk\Internal\Protocol\Frame;
use Mago\Sdk\Internal\Protocol\FrameCodec;
use Mago\Sdk\Internal\Protocol\FrameKind;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use Mago\Sdk\Internal\Worker\Protocol as WorkerProtocol;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;
use Mago\Sdk\WorkerReducer;
use Mago\Sdk\WorkerReductionContext;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;
use Revolt\EventLoop;
use RuntimeException;

use function array_diff;
use function array_keys;
use function fclose;
use function pack;

/**
 * @mago-expect lint:cyclomatic-complexity
 * @mago-expect lint:kan-defect
 * @mago-expect lint:too-many-methods
 */
final class WorkerTest extends TestCase
{
    private const ANALYZER_MAGIC = 0x4D41_4E41;
    private const CODEBASE_QUERY_RESPONSE = 0x8004;
    private const WORKER_MAGIC = 0x4D45_5854;
    private const WORKER_VERSION = 0x0001_0000;
    private const GENERATION = 1;
    private const ROUTE_CLASS = 'App\Http\Routes';
    private const ROUTE_METHOD = 'index';

    public function testExtensionIdentifiersAreCaseInsensitive(): void
    {
        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('registered more than once');

        new Worker(new Extension('Acme/Tools', 'Acme', '1.0.0'), new Extension('acme/tools', 'Acme', '2.0.0'));
    }

    public function testPluginAliasesAreUniqueAcrossExtensions(): void
    {
        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('selector `SHARED` is shared by plugins `first` and `second`');

        new Worker(new Extension('acme/first', 'First', '1.0.0', analyzerPlugins: [
            self::plugin(new PluginDefinition('first', 'First', 'First plugin.', ['shared'])),
        ]), new Extension('acme/second', 'Second', '1.0.0', analyzerPlugins: [
            self::plugin(new PluginDefinition('second', 'Second', 'Second plugin.', ['SHARED'])),
        ]));
    }

    public function testAWorkerStartsARequestOnlyAfterTheOneBeforeItResponds(): void
    {
        [$sent] = self::analyze(['a.php', 'b.php']);

        self::assertSame(
            [
                'describe: response',
                'a.php: start the hooks of a.php',
                'a.php: query',
                'a.php: response',
                'b.php: start the hooks of b.php',
                'b.php: response',
            ],
            $sent,
        );
    }

    public function testACancelledRequestThatHasNotStartedIsDroppedAndTheOneAfterItRuns(): void
    {
        [$sent, $routesByFile] = self::analyze(['a.php', 'b.php', 'c.php'], cancel: 'b.php');

        self::assertSame(
            [
                'describe: response',
                'a.php: start the hooks of a.php',
                'a.php: query',
                'a.php: response',
                'c.php: start the hooks of c.php',
                'c.php: response',
            ],
            $sent,
        );
        self::assertSame(['a.php', 'c.php'], array_keys($routesByFile));
    }

    public function testARuleThatCachesAroundACodebaseQueryGivesEveryFileTheWholeResult(): void
    {
        [, $routesByFile] = self::analyze(['a.php', 'b.php']);

        self::assertSame(['a.php' => ['App\Http\Routes::index'], 'b.php' => ['App\Http\Routes::index']], $routesByFile);
    }

    public function testACallbackThatThrowsCancelledWhileMagoCancelledNothingGetsAnErrorResponse(): void
    {
        $reducer = new class implements WorkerReducer {
            private bool $thrown = false;

            public function collect(): string
            {
                if (!$this->thrown) {
                    $this->thrown = true;

                    throw new CancelledException();
                }

                return '';
            }

            public function reduce(WorkerReductionContext $context): void {}
        };
        $worker = new Worker(new Extension('acme/metrics', 'Metrics', '1.0.0', workerReducer: $reducer));

        $sent = self::serve($worker, ['first' => self::collectRequest(), 'second' => self::collectRequest()]);

        self::assertSame(
            [
                'first: start the worker reducer of extension `acme/metrics`',
                'first: error The extension request was cancelled.',
                'second: start the worker reducer of extension `acme/metrics`',
                'second: response',
            ],
            $sent,
        );
    }

    /**
     * A request that runs several units starts the deadline again at each, and names each one, as a request of a
     * batch of files names each file. Mago names a request of one unit itself.
     */
    public function testAWorkerNamesEachUnitOfARequestAsItStartsIt(): void
    {
        $worker = new Worker(new Extension('acme/project', 'Project', '1.0.0', analyzerPlugins: [
            self::projectPlugin('first'),
            self::projectPlugin('second'),
        ]));

        $sent = self::serve($worker, [
            'describe' => self::describeRequest(),
            'initialize' => self::initializeRequest(),
            'scan' => self::codebaseScanRequest(),
            'before' => self::projectRequest(Protocol::BEFORE_ANALYSIS_REQUEST),
            'after' => self::projectRequest(Protocol::AFTER_ANALYSIS_REQUEST),
        ]);

        self::assertSame(
            [
                'describe: response',
                'initialize: start the initialization hooks of plugin `first`',
                'initialize: start the initialization hooks of plugin `second`',
                'initialize: response',
                'scan: start a codebase-scan hook of plugin `first`',
                'scan: start a codebase-scan hook of plugin `second`',
                'scan: response',
                'before: start the before-analysis hooks of plugin `first`',
                'before: start the before-analysis hooks of plugin `second`',
                'before: response',
                'after: start the after-analysis hooks of plugin `first`',
                'after: start the after-analysis hooks of plugin `second`',
                'after: response',
            ],
            $sent,
        );
    }

    /**
     * @return iterable<string, array{int}>
     */
    public static function magoMinorsOfAnotherProtocol(): iterable
    {
        $minor = Protocol::VERSION_U32 & 0xFFFF;
        yield 'an older mago' => [$minor - 1];
        yield 'a newer mago' => [$minor + 1];
    }

    /**
     * A mago of another analyzer protocol than this SDK's gets its first request refused, with the extension, both
     * protocols, the package the SDK comes from and the commands that make the two match. A mago before 0.6.1 prints
     * this text as the whole error.
     */
    #[DataProvider('magoMinorsOfAnotherProtocol')]
    public function testAWorkerRefusesAMagoOfAnotherProtocolNamingThePackage(int $magoMinor): void
    {
        $worker = new Worker(new Extension('acme/routes', 'Routes', '1.0.0'));
        $magoVersion = (Protocol::VERSION_U32 & 0xFFFF_0000) | $magoMinor;

        $sent = self::serve($worker, ['describe' => self::describeRequest($magoVersion)]);

        self::assertSame(
            [
                "describe: error The worker of `acme/routes` can't read mago's request: Mago speaks analyzer protocol "
                . "1.{$magoMinor}, and this worker's SDK, from `heyjordanparker/mago-sharp`, speaks 1.13. Run the mago "
                . 'of that release with `vendor/bin/mago`, or require the release of your mago with `composer require '
                . '--dev "heyjordanparker/mago-sharp:<the version mago --version prints>"`.',
            ],
            $sent,
        );
    }

    /**
     * A plugin `$identifier` whose initialization, codebase-scan, before-analysis and after-analysis hooks do nothing.
     *
     * @param non-empty-string $identifier
     */
    private static function projectPlugin(string $identifier): Plugin
    {
        return new class($identifier) implements
            Plugin,
            InitializationHook,
            CodebaseScanHook,
            BeforeAnalysisHook,
            AfterAnalysisHook {
            /**
             * @param non-empty-string $identifier
             */
            public function __construct(
                private readonly string $identifier,
            ) {}

            public function getDefinition(): PluginDefinition
            {
                return new PluginDefinition($this->identifier, $this->identifier, 'Does nothing.');
            }

            public function register(PluginRegistry $registry): void
            {
                $registry->registerInitializationHook($this);
                $registry->registerCodebaseScanHook($this);
                $registry->registerBeforeAnalysisHook($this);
                $registry->registerAfterAnalysisHook($this);
            }

            public function initialize(InitializationContext $context): void {}

            public function getTargets(): array
            {
                return ['App\*'];
            }

            public function scan(CodebaseScanContext $context): void {}

            public function beforeAnalysis(BeforeAnalysisContext $context): void {}

            public function afterAnalysis(AfterAnalysisContext $context): void {}
        };
    }

    /**
     * @param list<AfterFileAnalysisHook> $hooks
     */
    private static function plugin(PluginDefinition $definition, array $hooks = []): Plugin
    {
        return new class($definition, $hooks) implements Plugin {
            /**
             * @param list<AfterFileAnalysisHook> $hooks
             */
            public function __construct(
                private readonly PluginDefinition $definition,
                private readonly array $hooks,
            ) {}

            public function getDefinition(): PluginDefinition
            {
                return $this->definition;
            }

            public function register(PluginRegistry $registry): void
            {
                foreach ($this->hooks as $hook) {
                    $registry->registerAfterFileAnalysisHook($hook);
                }
            }
        };
    }

    /**
     * Runs a worker whose rule caches the routes one codebase query finds, as an app's rule does, while Mago sends
     * one after-file batch per file at once and cancels `$cancel`'s request when the first query arrives.
     *
     * @param non-empty-list<non-empty-string> $files
     *
     * @return array{list<string>, array<string, list<string>>} every frame the worker sent, in order, and the routes
     *     the rule saw for each file
     */
    private static function analyze(array $files, ?string $cancel = null): array
    {
        $rule = new class implements AfterFileAnalysisHook {
            /** @var array<string, list<string>> */
            public array $routesByFile = [];

            /** @var null|list<string> */
            private ?array $routes = null;

            public function getRequirements(): array
            {
                return [];
            }

            public function afterFileAnalysis(AfterFileAnalysisContext $context): void
            {
                if ($this->routes === null) {
                    $this->routes = [];
                    foreach ($context->codebase->findMethods(withAnyAttribute: ['Route'], fields: 0) as $route) {
                        $this->routes[] = "{$route->method->class}::{$route->method->member}";
                    }
                }

                $this->routesByFile[$context->analysis->file] = $this->routes;
            }
        };
        $worker = new Worker(new Extension('acme/routes', 'Routes', '1.0.0', analyzerPlugins: [
            self::plugin(new PluginDefinition('routes', 'Routes', 'Checks routes.'), [$rule]),
        ]));
        $requests = ['describe' => self::describeRequest()];
        foreach ($files as $file) {
            $requests[$file] = self::afterFileRequest($file);
        }

        return [self::serve($worker, $requests, $cancel), $rule->routesByFile];
    }

    /**
     * Runs `$worker` while Mago sends it every request in `$requests` at once, in order, and cancels the `$cancel`
     * request when the first codebase query arrives.
     *
     * @param non-empty-array<non-empty-string, string> $requests each request's payload, by a label for it
     *
     * @return list<string> every frame the worker sent, in order, named by its request's label
     */
    private static function serve(Worker $worker, array $requests, ?string $cancel = null): array
    {
        [$mago, $process] = StreamPair::create();
        $reader = new ResourceReader($mago);
        $writer = new ResourceWriter($mago);
        $sent = [];
        EventLoop::queue(static function () use ($reader, $writer, $requests, $cancel, &$sent): void {
            self::actAsMago($reader, $writer, $requests, $cancel, $sent);
        });
        $timeout = EventLoop::delay(2.0, static function (): never {
            throw new RuntimeException('The worker did not answer every request within two seconds.');
        });

        try {
            $worker->run($process, $process);
        } finally {
            EventLoop::cancel($timeout);
            $reader->close();
            $writer->close();
            fclose($mago);
            fclose($process);
        }

        return $sent;
    }

    /**
     * Sends the worker every request at once, answers every codebase query with one route, and shuts the worker down
     * once every request that was not cancelled has responded.
     *
     * @param non-empty-array<non-empty-string, string> $requests
     * @param list<string> $sent
     */
    private static function actAsMago(
        ResourceReader $reader,
        ResourceWriter $writer,
        array $requests,
        ?string $cancel,
        array &$sent,
    ): void {
        $codec = new FrameCodec();
        $labels = [];
        $id = 0;
        foreach ($requests as $label => $payload) {
            $labels[++$id] = $label;
            $writer->write($codec->encode(new Frame(FrameKind::Request, 0, $id, 0, $payload)));
        }

        $awaited = array_diff($labels, [$cancel]);
        while ($awaited !== []) {
            $frame = $codec->read($reader) ?? throw new RuntimeException('The worker closed its output.');
            if ($frame->kind === FrameKind::Response) {
                $sent[] = "{$labels[$frame->id]}: " . ($frame->flags === 0 ? 'response' : "error {$frame->payload}");
                unset($awaited[$frame->id]);
                continue;
            }

            if ($frame->kind === FrameKind::Notification) {
                $sent[] = "{$labels[$frame->parentId]}: start {$frame->payload}";
                continue;
            }

            $sent[] = "{$labels[$frame->parentId]}: query";
            if ($cancel !== null) {
                foreach (array_keys($labels, $cancel, true) as $cancelled) {
                    $writer->write($codec->encode(new Frame(FrameKind::Cancel, 0, $cancelled, 0, '')));
                }
                $cancel = null;
            }

            $writer->write($codec->encode(
                new Frame(FrameKind::Response, 0, $frame->id, $frame->parentId, self::routesResponse()),
            ));
        }

        $writer->write($codec->encode(new Frame(FrameKind::Shutdown, 0, 0, 0, '')));
    }

    private static function describeRequest(int $version = Protocol::VERSION_U32): string
    {
        $message = self::analyzerMessage(Protocol::DESCRIBE_REQUEST, $version);
        $message->writeU32(PHPVersion::fromParts(8, 4)->id);
        $message->writeCount(NodeKind::cases());
        foreach (NodeKind::cases() as $kind) {
            $message->writeString($kind->value);
        }

        return $message->finish();
    }

    /**
     * An after-file batch of `$file` alone. A batch runs several files' hooks, so the worker names each file as it
     * starts its hooks, which shows when each request started.
     */
    private static function afterFileRequest(string $file): string
    {
        $message = self::analyzerMessage(Protocol::AFTER_FILE_ANALYSIS_BATCH_REQUEST);
        $message->writeU64(self::GENERATION);
        $message->writeU16(1);
        $message->writeU16(0);
        $message->writeLength(1);
        $message->writeBytes($file);
        // The file's size, then its expression, inferred-return, yield-key, and yield-value counts.
        for ($count = 0; $count < 5; ++$count) {
            $message->writeU32(0);
        }
        // The file's reference summary.
        for ($count = 0; $count < 3; ++$count) {
            $message->writeU64(0);
        }
        $message->writeBoolean(false);
        $message->writeBoolean(false);

        return $message->finish();
    }

    /**
     * The initialization of the first plugin, then the second.
     */
    private static function initializeRequest(): string
    {
        $message = self::analyzerMessage(Protocol::INITIALIZE_REQUEST);
        $message->writeLength(2);
        $message->writeU16(0);
        $message->writeU16(1);

        return $message->finish();
    }

    /**
     * The only batch of a codebase scan, for the first plugin's codebase-scan hook, then the second's, with no file.
     */
    private static function codebaseScanRequest(): string
    {
        $message = self::analyzerMessage(Protocol::CODEBASE_SCAN_REQUEST);
        $message->writeBoolean(true);
        $message->writeBoolean(true);
        $message->writeLength(2);
        $message->writeU16(0);
        $message->writeU16(1);
        $message->writeLength(0);

        return $message->finish();
    }

    /**
     * A before-analysis or after-analysis request of `$kind` for the first plugin, then the second. An after-analysis
     * request carries a project of no file and no issue.
     */
    private static function projectRequest(int $kind): string
    {
        $message = self::analyzerMessage($kind);
        $message->writeU64(self::GENERATION);
        $message->writeU16(2);
        $message->writeU16(0);
        $message->writeU16(1);
        if ($kind === Protocol::AFTER_ANALYSIS_REQUEST) {
            $message->writeU32(0);
            // The project's reference summary.
            for ($count = 0; $count < 3; ++$count) {
                $message->writeU64(0);
            }
            $message->writeLength(0);
        }

        return $message->finish();
    }

    private static function routesResponse(): string
    {
        $message = self::analyzerMessage(self::CODEBASE_QUERY_RESPONSE);
        $message->writeU64(self::GENERATION);
        $message->writeU8(Protocol::FIND_METHODS);
        $message->writeU32(0);
        $message->writeLength(1);
        $message->writeBytes(self::ROUTE_CLASS);
        $message->writeBytes(self::ROUTE_METHOD);
        // The method's function-like identifier: kind 2, a method.
        $message->writeU8(2);
        $message->writeBytes(self::ROUTE_CLASS);
        $message->writeBytes(self::ROUTE_METHOD);

        return $message->finish();
    }

    private static function analyzerMessage(int $kind, int $version = Protocol::VERSION_U32): PayloadWriter
    {
        return new PayloadWriter(pack('N3', self::ANALYZER_MAGIC, $version, $kind << 16));
    }

    private static function collectRequest(): string
    {
        return pack('N3', self::WORKER_MAGIC, self::WORKER_VERSION, WorkerProtocol::COLLECT_REQUEST << 16);
    }
}
