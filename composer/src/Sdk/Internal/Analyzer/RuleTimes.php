<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Internal\Protocol\ValueWriter;

use function count;
use function hrtime;

/**
 * How long each rule's own calls took in one request, by the rule's class and method, then by the file they ran on.
 * Mago asks for them when it logs at the DEBUG level, and logs each rule's time in the round.
 *
 * @internal
 */
final class RuleTimes
{
    /** @var array<string, array<array-key, int>> nanoseconds by rule, then by file, '' for a call on the project */
    private array $nanoseconds = [];

    private int $started = 0;

    /**
     * Starts the clock of the call that follows.
     */
    public function start(): void
    {
        $this->started = (int) hrtime(true);
    }

    /**
     * Adds the time since `start()` to `$method` of `$rule`, on `$file`, or on the project when there is none.
     */
    public function stop(object $rule, string $method, ?string $file): void
    {
        $took = (int) hrtime(true) - $this->started;
        $name = $rule::class . '::' . $method;
        $file ??= '';
        $this->nanoseconds[$name][$file] = ($this->nanoseconds[$name][$file] ?? 0) + $took;
    }

    /**
     * Writes a count, then each rule's time on each file: the rule, the file or none, and the nanoseconds.
     */
    public function write(ValueWriter $writer): void
    {
        $count = 0;
        foreach ($this->nanoseconds as $files) {
            $count += count($files);
        }

        $writer->writeLength($count);
        foreach ($this->nanoseconds as $rule => $files) {
            foreach ($files as $file => $nanoseconds) {
                $file = (string) $file;
                $writer->writeString($rule);
                $writer->writeOptionalString($file === '' ? null : $file);
                $writer->writeU64($nanoseconds);
            }
        }
    }
}
