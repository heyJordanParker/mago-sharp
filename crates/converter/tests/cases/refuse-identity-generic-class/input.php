<?php

declare(strict_types=1);

namespace App;

// PHP's `$value::class` is a class's name, and PHP#'s `Class.of(value)` carries a `Box<int>`'s type arguments, so
// `Class.of(value) == typeof(Box)` is false where PHP's `===` is true. `===` of class values refuses where a side may be
// such a class and neither names a class that takes no type arguments (spec section 25).
final class Check
{
    public function isBox(object $value): bool
    {
        return $value::class === Box::class;
    }

    public function sameClass(object $a, object $b): bool
    {
        return $a::class === $b::class;
    }

    public function isCheck(object $value): bool
    {
        return $value::class === Check::class;
    }
}
