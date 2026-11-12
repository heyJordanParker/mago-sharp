<?php

declare(strict_types=1);

namespace App;

/** @implements \ArrayAccess<string, int> */
final class Order implements \ArrayAccess
{
    /** @var array<string, int> */
    private array $lines = [];

    public function offsetExists(mixed $offset): bool
    {
        return isset($this->lines[$offset]);
    }

    public function offsetGet(mixed $offset): int
    {
        return $this->lines[$offset];
    }

    public function offsetSet(mixed $offset, mixed $value): void
    {
        $this->lines[$offset] = $value;
    }

    public function offsetUnset(mixed $offset): void
    {
        unset($this->lines[$offset]);
    }
}
