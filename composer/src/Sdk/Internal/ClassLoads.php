<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal;

use Mago\Sdk\Internal\Protocol\PayloadWriter;
use ReflectionClass;

use function array_slice;
use function class_exists;
use function count;
use function get_declared_classes;
use function get_declared_interfaces;
use function get_declared_traits;
use function interface_exists;
use function max;
use function random_int;
use function spl_autoload_register;
use function str_ends_with;
use function strtolower;
use function trait_exists;

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

    /** @var list<string> each entry's lowercase class name */
    private array $log = [];

    /** @var array<string, true> */
    private array $logged = [];

    /** @var list<string> the classes asked of the autoloaders since the log last caught up */
    private array $asked = [];

    /** @var array{int, int, int} how many declared classes, interfaces and traits the log has seen */
    private array $seen = [0, 0, 0];

    /** How many entries Mago has been sent. */
    private int $sent = 0;

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
     * end the callbacks that just finished read up to.
     */
    public function end(): int
    {
        foreach ([get_declared_classes(), get_declared_interfaces(), get_declared_traits()] as $kind => $declared) {
            foreach (array_slice($declared, $this->seen[$kind]) as $class) {
                $file = (new ReflectionClass($class))->getFileName();
                if ($file !== false && str_ends_with($file, '.sharp')) {
                    $this->add($class);
                }
            }

            $this->seen[$kind] = count($declared);
        }

        foreach ($this->asked as $class) {
            if (!class_exists($class, false) && !interface_exists($class, false) && !trait_exists($class, false)) {
                $this->add($class);
            }
        }

        $this->asked = [];

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
        foreach ($classes as $class) {
            if (isset($this->logged[$class])) {
                return true;
            }
        }

        return false;
    }

    private function add(string $class): void
    {
        $class = strtolower($class);
        if (isset($this->logged[$class])) {
            return;
        }

        $this->logged[$class] = true;
        $this->log[] = $class;
    }
}
