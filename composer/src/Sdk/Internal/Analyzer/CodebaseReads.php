<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use function array_fill_keys;
use function array_keys;
use function explode;
use function hash_final;
use function hash_init;
use function hash_update;
use function implode;

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
    /** @var array<string, array<array-key, true>> each read's name, by its `"{operation}:{argument}"` */
    private array $reads = [];

    public function record(int $operation, int $argument, string $name): void
    {
        $this->reads["{$operation}:{$argument}"][$name] = true;
    }

    /**
     * Records a read of each of `$names` by the same operation and argument, as one query of several names reads them.
     *
     * @param list<string> $names
     */
    public function recordAll(int $operation, int $argument, array $names): void
    {
        $read = array_fill_keys($names, true);
        $held = $this->reads["{$operation}:{$argument}"] ?? null;
        $this->reads["{$operation}:{$argument}"] = $held === null ? $read : $held + $read;
    }

    /**
     * The reads' identity: the XXH128 of the reads in the order they were made, which two lists share when they hold
     * the same reads in the same order.
     */
    public function key(): string
    {
        $hash = hash_init('xxh128');
        foreach ($this->reads as $query => $names) {
            hash_update($hash, "\1{$query}\1" . implode("\0", array_keys($names)));
        }

        return hash_final($hash, true);
    }

    /**
     * @return list<array{int, int, string}>
     */
    public function reads(): array
    {
        $reads = [];
        foreach ($this->reads as $query => $names) {
            [$operation, $argument] = explode(':', $query);
            foreach ($names as $name => $_) {
                $reads[] = [(int) $operation, (int) $argument, (string) $name];
            }
        }

        return $reads;
    }
}
