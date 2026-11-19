<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\ClassLikeAnalysisHook;
use Mago\Sdk\Analyzer\ClassLikeTarget;
use Mago\Sdk\Analyzer\CodebaseScanContext;
use Mago\Sdk\Analyzer\CodebaseScanHook;
use Mago\Sdk\Analyzer\InitializationContext;
use Mago\Sdk\Analyzer\InitializationHook;
use Mago\Sdk\Analyzer\MethodCallAnalysisHook;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Extension;
use Mago\Sdk\Internal\Analyzer\Protocol;
use Mago\Sdk\Internal\Hosts;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use Mago\Sdk\Internal\Worker\Protocol as WorkerProtocol;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;
use Mago\Sdk\WorkerReducer;
use Mago\Sdk\WorkerReductionContext;
use PHPUnit\Framework\TestCase;
use ReflectionProperty;
use RuntimeException;

use function count;
use function pack;

require_once __DIR__ . '/Engine.php';

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

    /**
     * A hook's targets name class-likes and methods as written, since only Mago knows a class's rule: `Box::open` and
     * `Box::Open` are two methods of a PHP# class, and `Demo\Order` and `Demo\ORDER` two PHP# class-likes, spec
     * section 24. Only one name written twice is refused.
     */
    public function testAHookTargetsTwoNamesThatDifferOnlyInCase(): void
    {
        $worker = new Worker(new Extension('acme/twins', 'Twins', '1.0.0', analyzerPlugins: [
            self::targetingPlugin([MethodTarget::exact('Demo\Box', 'open'), MethodTarget::exact('Demo\Box', 'Open')], [
                ClassLikeTarget::descendantsOf('Demo\Order'),
                ClassLikeTarget::descendantsOf('Demo\ORDER'),
            ]),
        ]));
        self::assertInstanceOf(Worker::class, $worker);

        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage(
            'A analyzer method-call analysis hook in `twins` targets `Demo\Box::open` more than once.',
        );

        new Worker(new Extension('acme/twins', 'Twins', '1.0.0', analyzerPlugins: [
            self::targetingPlugin([MethodTarget::exact('Demo\Box', 'open'), MethodTarget::exact('Demo\Box', 'open')], [
                ClassLikeTarget::descendantsOf('Demo\Order'),
            ]),
        ]));
    }

    public function testARuleThatCachesAroundACodebaseQueryGivesEveryFileTheWholeResult(): void
    {
        [, $routesByFile] = self::analyze(['a.php', 'b.php']);

        self::assertSame(['a.php' => ['App\Http\Routes::index'], 'b.php' => ['App\Http\Routes::index']], $routesByFile);
    }

    public function testACallbackThatThrowsFailsItsRequestAndTheNextOneRuns(): void
    {
        $reducer = new class implements WorkerReducer {
            private bool $thrown = false;

            public function collect(): string
            {
                if (!$this->thrown) {
                    $this->thrown = true;

                    throw new RuntimeException('The metrics store is closed.');
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
                'first: error Worker reducer for extension `acme/metrics` failed to collect: The metrics store is closed.',
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
     * A plugin `twins` with one method-call hook of `$methods` and one class-like hook of `$classLikes`.
     *
     * @param non-empty-list<MethodTarget> $methods
     * @param non-empty-list<ClassLikeTarget> $classLikes
     */
    private static function targetingPlugin(array $methods, array $classLikes): Plugin
    {
        return new class($methods, $classLikes) implements Plugin {
            /**
             * @param non-empty-list<MethodTarget> $methods
             * @param non-empty-list<ClassLikeTarget> $classLikes
             */
            public function __construct(
                private readonly array $methods,
                private readonly array $classLikes,
            ) {}

            public function getDefinition(): PluginDefinition
            {
                return new PluginDefinition('twins', 'Twins', 'Hooks names that differ only in case.');
            }

            public function register(PluginRegistry $registry): void
            {
                $registry->registerMethodCallAnalysisHook(new class($this->methods) implements MethodCallAnalysisHook {
                    /** @param non-empty-list<MethodTarget> $targets */
                    public function __construct(
                        private readonly array $targets,
                    ) {}

                    public function getTargets(): array
                    {
                        return $this->targets;
                    }

                    public function getRequirements(): array
                    {
                        return [];
                    }

                    public function analyze(NodeAnalysisContext $context): void {}
                });
                $registry->registerClassLikeAnalysisHook(new class($this->classLikes) implements ClassLikeAnalysisHook {
                    /** @param non-empty-list<ClassLikeTarget> $targets */
                    public function __construct(
                        private readonly array $targets,
                    ) {}

                    public function getTargets(): array
                    {
                        return $this->targets;
                    }

                    public function getRequirements(): array
                    {
                        return [];
                    }

                    public function analyze(NodeAnalysisContext $context): void {}
                });
            }
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
     * one after-file batch per file.
     *
     * @param non-empty-list<non-empty-string> $files
     *
     * @return array{list<string>, array<string, list<string>>} everything the worker told Mago, in order, and the routes
     *     the rule saw for each file
     */
    private static function analyze(array $files): array
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

        return [self::serve($worker, $requests), $rule->routesByFile];
    }

    /**
     * Registers `$worker` as mago's engine runs a host's file, then calls it with every request in `$requests`, in
     * order, answering each codebase query with one route.
     *
     * @param non-empty-array<non-empty-string, string> $requests each request's payload, by a label for it
     *
     * @return list<string> everything the worker told mago, in order, named by its request's label
     */
    private static function serve(Worker $worker, array $requests): array
    {
        /** @var list<\Closure(string): string> $handlers */
        $handlers = (new ReflectionProperty(Hosts::class, 'handlers'))->getValue();
        $place = count($handlers);
        $worker->run();
        $sent = [];
        foreach ($requests as $label => $payload) {
            Mago::$answer = static function (string $question, string $asked) use ($label, &$sent): ?string {
                $sent[] = $question === 'progress' ? "{$label}: start {$asked}" : "{$label}: query";

                return $question === 'progress' ? null : self::routesResponse();
            };
            [$served, $response] = Hosts::handle($place, $payload);
            $sent[] = "{$label}: " . ($served ? 'response' : "error {$response}");
        }

        Mago::$answer = null;

        return $sent;
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
        // The method's function-like identifier: kind 2, a method, of a plain PHP class, rule 0.
        $message->writeU8(2);
        $message->writeBytes(self::ROUTE_CLASS);
        $message->writeBytes(self::ROUTE_METHOD);
        $message->writeU8(0);

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
