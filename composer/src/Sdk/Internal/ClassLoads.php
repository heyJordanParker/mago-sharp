<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal;

use Mago\Sdk\Internal\Protocol\PayloadWriter;
use ReflectionClass;

use function array_diff;
use function array_filter;
use function array_intersect;
use function array_map;
use function array_merge;
use function array_slice;
use function array_unique;
use function array_values;
use function count;
use function get_declared_classes;
use function get_declared_interfaces;
use function get_declared_traits;
use function Mago\Engine\classCount;
use function max;
use function random_int;
use function spl_autoload_register;
use function str_ends_with;
use function strtolower;

use const PHP_INT_MAX;

/**
 * The log of the classes this worker declared from a `.sharp` file and of the classes it was asked for and never
 * declared, in the order it met them. A hook or provider may keep what any class its worker loaded before it ran left
 * behind, so the callbacks of a file read every entry before the end Mago gets with their reads, and Mago starts the
 * worker again when the compiled file of one of them changes.
 *
 * @internal
 */
final class ClassLoads
{
    /** The id this worker drew at its start, which names its log to Mago. */
    private readonly int $worker;

    /** @var list<lowercase-string> each entry's lowercase class name */
    private array $log = [];

    /** @var list<string> the classes asked of the autoloaders since the log last caught up */
    private array $asked = [];

    /** @var list<int> how many declared classes, interfaces and traits the log has seen */
    private array $seen = [0, 0, 0];

    /** How many entries Mago has been sent. */
    private int $sent = 0;

    /** How many class-likes the interpreter had declared when the log last caught up. */
    private int $classCount = -1;

    public function __construct()
    {
        $this->worker = random_int(1, PHP_INT_MAX);
        spl_autoload_register(
            function (string $class): void {
                $this->asked[] = $class;
            },
            true,
            true,
        );
    }

    /**
     * Catches the log up with what the worker declared and failed to load, and returns how many entries it holds: the
     * end the callbacks that just finished read up to. The autoloaders are asked only for a name no class, interface
     * or trait holds, so a name asked since the last catch-up that none of them declared since is one that never
     * loaded. With no class-like declared and no name asked since, the log already holds everything.
     */
    public function end(): int
    {
        $classCount = classCount();
        if ($classCount === $this->classCount && $this->asked === []) {
            return count($this->log);
        }

        $this->classCount = $classCount;
        $declared = [get_declared_classes(), get_declared_interfaces(), get_declared_traits()];
        $new = array_merge(...array_map(
            /**
             * @param list<class-string> $names
             *
             * @return list<class-string>
             */
            static fn(array $names, int $seen): array => array_slice($names, $seen),
            $declared,
            $this->seen,
        ));
        $this->seen = array_map(count(...), $declared);
        $sharp = array_filter(
            $new,
            /** @param class-string $class */
            static fn(string $class): bool => str_ends_with(
                (string) (new ReflectionClass($class))->getFileName(),
                '.sharp',
            ),
        );
        $missing = array_diff(array_map(strtolower(...), $this->asked), array_map(strtolower(...), $new));
        $this->asked = [];
        $this->log = array_values(array_unique([...$this->log, ...array_map(strtolower(...), $sharp), ...$missing]));

        return count($this->log);
    }

    /**
     * Writes the entries Mago has not been sent, up to `$end`: the worker's id, the place of the first one in the log,
     * and each one's name.
     */
    public function write(PayloadWriter $writer, int $end): void
    {
        $entries = array_slice($this->log, $this->sent, max(0, $end - $this->sent));
        $writer->writeU64($this->worker);
        $writer->writeU32($this->sent);
        $writer->writeCount($entries);
        foreach ($entries as $class) {
            $writer->writeBytes($class);
        }

        $this->sent += count($entries);
    }

    /**
     * Whether the log holds one of `$classes`, each a lowercase class name.
     *
     * @param list<string> $classes
     */
    public function holdsAny(array $classes): bool
    {
        $this->end();

        return array_intersect($classes, $this->log) !== [];
    }
}
