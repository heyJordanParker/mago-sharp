<?php

declare(strict_types=1);

namespace Acme\Format;

/**
 * A collection PHP reads and writes by index through `ArrayAccess`, as a framework's collection does, which counts its
 * writes and removals so a run shows `offsetSet` and `offsetUnset` ran.
 *
 * @implements \ArrayAccess<string, int>
 */
final class Bag implements \ArrayAccess
{
    /** @var array<string, int> */
    private array $items = [];

    public int $writes = 0;

    public int $removals = 0;

    public function offsetExists(mixed $offset): bool
    {
        return isset($this->items[$offset]);
    }

    public function offsetGet(mixed $offset): int
    {
        return $this->items[$offset];
    }

    public function offsetSet(mixed $offset, mixed $value): void
    {
        $this->writes++;
        $this->items[$offset] = $value;
    }

    public function offsetUnset(mixed $offset): void
    {
        $this->removals++;
        unset($this->items[$offset]);
    }
}
