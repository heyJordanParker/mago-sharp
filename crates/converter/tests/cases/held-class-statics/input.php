<?php

declare(strict_types=1);

namespace App;

// A static property, a constant and a static method value of a class held at run time, which PHP# reaches through
// reflection: a read and a write through `kind.properties()`, a read through `kind.constants()`, and a lambda over
// `kind.methods()` (spec sections 14.3 and 25).
final class Reader
{
    /** @param class-string<Order> $kind */
    public function count(string $kind): int
    {
        return $kind::$count + 1;
    }

    /** @param class-string<Order> $kind */
    public function limit(string $kind): int
    {
        return $kind::MAX * 2;
    }

    /** @param class-string<Order> $kind */
    public function reset(string $kind): void
    {
        $kind::$count = 0;
    }

    /** @param class-string<Order> $kind */
    public function first(string $kind): ?Order
    {
        $find = $kind::find(...);

        return $find(1);
    }
}
