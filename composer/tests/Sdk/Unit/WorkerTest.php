<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Extension;
use Mago\Sdk\Internal\Analyzer\Protocol;
use Mago\Sdk\Internal\Io\ResourceReader;
use Mago\Sdk\Internal\Io\ResourceWriter;
use Mago\Sdk\Internal\Protocol\Frame;
use Mago\Sdk\Internal\Protocol\FrameCodec;
use Mago\Sdk\Internal\Protocol\FrameKind;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;
use PHPUnit\Framework\TestCase;
use Revolt\EventLoop;
use RuntimeException;

use function array_diff;
use function array_keys;
use function fclose;
use function pack;
use function stream_socket_pair;

use const PHP_OS_FAMILY;
use const STREAM_IPPROTO_IP;
use const STREAM_PF_INET;
use const STREAM_PF_UNIX;
use const STREAM_SOCK_STREAM;

/**
 * @mago-expect lint:cyclomatic-complexity
 * @mago-expect lint:too-many-methods
 */
final class WorkerTest extends TestCase
{
    private const ANALYZER_MAGIC = 0x4D41_4E41;
    private const CODEBASE_QUERY_RESPONSE = 0x8004;
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

        self::assertSame(['a.php: start', 'a.php: query', 'a.php: response', 'b.php: start', 'b.php: response'], $sent);
    }

    public function testACancelledRequestThatHasNotStartedIsDroppedAndTheOneAfterItRuns(): void
    {
        [$sent, $routesByFile] = self::analyze(['a.php', 'b.php', 'c.php'], cancel: 'b.php');

        self::assertSame(['a.php: start', 'a.php: query', 'a.php: response', 'c.php: start', 'c.php: response'], $sent);
        self::assertSame(['a.php', 'c.php'], array_keys($routesByFile));
    }

    public function testARuleThatCachesAroundACodebaseQueryGivesEveryFileTheWholeResult(): void
    {
        [, $routesByFile] = self::analyze(['a.php', 'b.php']);

        self::assertSame(['a.php' => ['App\Http\Routes::index'], 'b.php' => ['App\Http\Routes::index']], $routesByFile);
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
     * one after-file request per file at once and cancels `$cancel`'s request when the first query arrives.
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
        [$mago, $process] = self::createStreamPair();
        $sent = [];
        EventLoop::queue(static function () use ($mago, $files, $cancel, &$sent): void {
            self::actAsMago($mago, $files, $cancel, $sent);
        });
        $timeout = EventLoop::delay(2.0, static function (): never {
            throw new RuntimeException('The worker did not answer every request within two seconds.');
        });

        $worker->run($process, $process);

        EventLoop::cancel($timeout);
        fclose($mago);
        fclose($process);

        return [$sent, $rule->routesByFile];
    }

    /**
     * Sends the worker one after-file request per file, answers every codebase query with one route, and shuts the
     * worker down once every request that was not cancelled has responded.
     *
     * @param resource $stream
     * @param non-empty-list<non-empty-string> $files
     * @param list<string> $sent
     */
    private static function actAsMago(mixed $stream, array $files, ?string $cancel, array &$sent): void
    {
        $codec = new FrameCodec();
        $reader = new ResourceReader($stream);
        $writer = new ResourceWriter($stream);
        $writer->write($codec->encode(new Frame(FrameKind::Request, 0, 1, 0, self::describeRequest())));
        $codec->read($reader);

        $requests = [];
        foreach ($files as $index => $file) {
            $requests[$index + 2] = $file;
            $writer->write($codec->encode(
                new Frame(FrameKind::Request, 0, $index + 2, 0, self::afterFileRequest($file)),
            ));
        }

        $awaited = array_diff($requests, [$cancel]);
        while ($awaited !== []) {
            $frame = $codec->read($reader) ?? throw new RuntimeException('The worker closed its output.');
            if ($frame->kind === FrameKind::Response) {
                $sent[] = "{$requests[$frame->id]}: " . ($frame->flags === 0 ? 'response' : "error {$frame->payload}");
                unset($awaited[$frame->id]);
                continue;
            }

            if ($frame->kind === FrameKind::Notification) {
                $sent[] = "{$requests[$frame->parentId]}: start";
                continue;
            }

            $sent[] = "{$requests[$frame->parentId]}: query";
            if ($cancel !== null) {
                foreach (array_keys($requests, $cancel, true) as $cancelled) {
                    $writer->write($codec->encode(new Frame(FrameKind::Cancel, 0, $cancelled, 0, '')));
                }
                $cancel = null;
            }

            $writer->write($codec->encode(
                new Frame(FrameKind::Response, 0, $frame->id, $frame->parentId, self::routesResponse()),
            ));
        }

        $writer->write($codec->encode(new Frame(FrameKind::Shutdown, 0, 0, 0, '')));
        $reader->close();
        $writer->close();
    }

    private static function describeRequest(): string
    {
        $message = self::analyzerMessage(Protocol::DESCRIBE_REQUEST);
        $message->writeU32(PHPVersion::fromParts(8, 4)->id);
        $message->writeCount(NodeKind::cases());
        foreach (NodeKind::cases() as $kind) {
            $message->writeString($kind->value);
        }

        return $message->finish();
    }

    private static function afterFileRequest(string $file): string
    {
        $message = self::analyzerMessage(Protocol::AFTER_FILE_ANALYSIS_REQUEST);
        $message->writeU64(self::GENERATION);
        $message->writeU16(1);
        $message->writeU16(0);
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

    private static function analyzerMessage(int $kind): PayloadWriter
    {
        return new PayloadWriter(pack('N3', self::ANALYZER_MAGIC, Protocol::VERSION_U32, $kind << 16));
    }

    /**
     * @return array{resource, resource}
     */
    private static function createStreamPair(): array
    {
        $domain = PHP_OS_FAMILY === 'Windows' ? STREAM_PF_INET : STREAM_PF_UNIX;
        $streams = stream_socket_pair($domain, STREAM_SOCK_STREAM, STREAM_IPPROTO_IP);
        if ($streams === false) {
            throw new RuntimeException('Unable to create SDK test streams.');
        }

        return $streams;
    }
}
