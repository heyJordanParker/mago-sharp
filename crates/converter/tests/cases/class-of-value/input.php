<?php

declare(strict_types=1);

namespace App;

// `$order::class` is the class of the object, which PHP# writes `Class.of(order)`, since `typeof` takes a type only
// (spec section 25). The engine gives both the same class name.
final class Ledger
{
    public function kind(Order $order): string
    {
        return $order::class;
    }

    public function own(): string
    {
        return self::class;
    }
}
