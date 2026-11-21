<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use function array_fill_keys;
use function hash;
use function serialize;

/**
 * The codebase reads one file's hooks and providers made, so Mago runs them again when what they
 * read changes.
 *
 * A read is the codebase query that answers it: a class-like, function, or constant query names
 * one symbol, and a listing query names the set of names it lists.
 *
 * @internal
 */
final class CodebaseReads
{
    /** @var array<int, array<int, array<array-key, true>>> each read's name, by its operation and argument */
    private array $reads = [];

    public function record(int $operation, int $argument, string $name): void
    {
        $this->reads[$operation][$argument][$name] = true;
    }

    /**
     * Records a read of each of `$names` by the same operation and argument, as one query of several names reads them.
     *
     * @param list<string> $names
     */
    public function recordAll(int $operation, int $argument, array $names): void
    {
        $this->reads[$operation][$argument] =
            ($this->reads[$operation][$argument] ?? []) + array_fill_keys($names, true);
    }

    /**
     * The reads' identity: the XXH128 of the reads in the order they were made, which two lists share when they hold
     * the same reads in the same order.
     */
    public function key(): string
    {
        return hash('xxh128', serialize($this->reads), true);
    }

    /**
     * @return list<array{int, int, string}>
     */
    public function reads(): array
    {
        $reads = [];
        foreach ($this->reads as $operation => $arguments) {
            foreach ($arguments as $argument => $names) {
                foreach ($names as $name => $_) {
                    $reads[] = [$operation, $argument, (string) $name];
                }
            }
        }

        return $reads;
    }
}
