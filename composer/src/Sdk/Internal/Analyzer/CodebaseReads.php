<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use function array_values;

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
     * @return list<array{int, int, string}>
     */
    public function take(): array
    {
        $reads = array_values($this->reads);
        $this->reads = [];

        return $reads;
    }
}
