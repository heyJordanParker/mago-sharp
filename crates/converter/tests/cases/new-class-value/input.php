<?php

declare(strict_types=1);

namespace App;

// `new $type(…)` of a class value is `new (type)(…)` (spec section 25).
final class Maker
{
    /** @param class-string<Widget> $type */
    public function make(string $type, string $name): Widget
    {
        return new $type($name);
    }
}
