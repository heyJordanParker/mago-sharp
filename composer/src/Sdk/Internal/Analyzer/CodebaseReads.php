<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use function array_keys;
use function array_values;
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
    /** @var array<string, array{int, int, string}> */
    private array $reads = [];

    public function record(int $operation, int $argument, string $name): void
    {
        $this->reads["{$operation}\0{$argument}\0{$name}"] ??= [$operation, $argument, $name];
    }

    /**
     * The reads' identity, which two lists share exactly when they hold the same reads in the same order.
     */
    public function key(): string
    {
        return implode("\1", array_keys($this->reads));
    }

    /**
     * @return list<array{int, int, string}>
     */
    public function reads(): array
    {
        return array_values($this->reads);
    }
}
