<?php

declare(strict_types=1);

namespace App;

// PHP# reaches a static through its class's name or a type parameter, so a static of a `class-string<T>` is `T`'s, as
// `T.find(7)`, and a static call through a class held at run time is a call through reflection's invoker, which gives
// the result the method declares and lists the effects its `extern` line does (spec sections 11, 25 and 29).
final class Finder
{
    /**
     * @template T of Order
     * @param class-string<T> $kind
     */
    public function total(string $kind): int
    {
        $found = $kind::find(7);
        $make = $kind::find(...);
        $make(1);

        return $kind::$count + $kind::MAX + ($found === null ? 0 : $found->id);
    }

    /** @param class-string<Order> $kind */
    public function label(string $kind): string
    {
        return $kind::label(7);
    }

    /** @param class-string<Order> $kind */
    public function code(string $kind): string
    {
        return $kind::code(7);
    }
}
