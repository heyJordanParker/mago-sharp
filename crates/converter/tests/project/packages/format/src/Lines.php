<?php

declare(strict_types=1);

namespace Acme\Format;

/**
 * A plain PHP collection, as a framework's is, which PHP loops over through its iterator.
 *
 * @implements \IteratorAggregate<int, string>
 */
final class Lines implements \IteratorAggregate
{
    /** @param list<string> $lines */
    public function __construct(private array $lines)
    {
    }

    /** @return \ArrayIterator<int, string> */
    public function getIterator(): \ArrayIterator
    {
        return new \ArrayIterator($this->lines);
    }

    /**
     * Calls `$each` with each line and its index, as a framework's `map` does.
     *
     * @param \Closure(string, int): string $each
     * @return list<string>
     */
    public function mapped(\Closure $each): array
    {
        $mapped = [];
        foreach ($this->lines as $index => $line) {
            $mapped[] = $each($line, $index);
        }

        return $mapped;
    }
}
