<?php

declare(strict_types=1);

namespace App;

// PHP# reaches a static through its class's name or a type parameter, and a static of a class held at run time
// through reflection, which reads no constant and needs the method the analysis finds to type the call (spec sections
// 11 and 25).
final class Reader
{
    /** @param class-string<Order> $kind */
    public function count(string $kind): int
    {
        return $kind::$count + $kind::MAX;
    }

    /** @param class-string<Order> $kind */
    public function first(string $kind): ?Order
    {
        $find = $kind::find(...);

        return $find(1);
    }

    /** @param class-string $kind */
    public function any(string $kind): mixed
    {
        return $kind::find(7);
    }
}
