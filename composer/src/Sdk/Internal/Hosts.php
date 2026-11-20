<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal;

use Closure;
use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Analyzer\SentReads;
use Throwable;

use function class_exists;
use function count;
use function enum_exists;
use function function_exists;
use function interface_exists;
use function trait_exists;

/**
 * The extension hosts one interpreter of mago's engine runs: the request handler of the worker each host's file
 * registered, in the order mago loaded the files, and the interpreter's one class log. mago calls them here, on the
 * analysis thread the interpreter belongs to.
 *
 * @internal
 */
final class Hosts
{
    /** @var list<Closure(list<null|bool|int|float|string|list<mixed>>): list<null|bool|int|float|string|list<mixed>>> */
    private static array $handlers = [];

    private static ?ClassLoads $classLoads = null;

    private static ?SentReads $sentReads = null;

    /**
     * Registers the request handler of the worker a host's file runs.
     *
     * @param Closure(list<null|bool|int|float|string|list<mixed>>): list<null|bool|int|float|string|list<mixed>> $handler
     */
    public static function register(Closure $handler): void
    {
        self::$handlers[] = $handler;
    }

    /**
     * Runs the file of the extension host `$name`, which registers one worker, the host's next.
     *
     * @throws ProtocolException when the file registers no worker or more than one.
     */
    public static function load(string $name, string $file): void
    {
        $before = count(self::$handlers);
        (static function (string $file): void {
            require $file;
        })($file);

        if (count(self::$handlers) !== ($before + 1)) {
            throw new ProtocolException(
                "The file of extension host `{$name}`, {$file}, must call Mago\\Sdk\\Worker::run() once.",
            );
        }
    }

    /**
     * Runs the files of `$hosts`, each a host's name and file, in order, up to the first after which the class-like or
     * function `$class` is declared, and returns that host's place: the host that declared it.
     *
     * @param list<array{string, string}> $hosts
     */
    public static function declarer(array $hosts, string $class): int
    {
        foreach ($hosts as $place => [$name, $file]) {
            self::load($name, $file);
            if (
                class_exists($class, false)
                || interface_exists($class, false)
                || trait_exists($class, false)
                || enum_exists($class, false)
                || function_exists($class)
            ) {
                return $place;
            }
        }

        return -1;
    }

    /**
     * Serves request `$payload` with the worker of the host at `$host`: true and the response, or false and why the
     * request failed. A request and its response are each a list of values.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{true, list<null|bool|int|float|string|list<mixed>>}|array{false, string}
     */
    public static function handle(int $host, array $payload): array
    {
        try {
            return [true, self::$handlers[$host]($payload)];
        } catch (Throwable $throwable) {
            return [false, $throwable->getMessage()];
        }
    }

    /** The class log of this interpreter, which every worker in it shares. */
    public static function classLoads(): ClassLoads
    {
        return self::$classLoads ??= new ClassLoads();
    }

    /** The codebase reads this interpreter sent Mago, which every worker in it shares, as it shares the class log. */
    public static function sentReads(): SentReads
    {
        return self::$sentReads ??= new SentReads();
    }
}
