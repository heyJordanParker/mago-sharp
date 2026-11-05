<?php

declare(strict_types=1);

namespace App;

// A type PHP# can't write keeps the file plain PHP, with the type named (spec section 24).
final class Types
{
    public function find(): int|false
    {
        return false;
    }

    /** @param array-key $key */
    public function key($key): void
    {
    }

    public function rows(): array
    {
        return [];
    }

    public function both(\Countable&\Stringable $value): void
    {
    }

    public function call(callable $callback): void
    {
    }
}
