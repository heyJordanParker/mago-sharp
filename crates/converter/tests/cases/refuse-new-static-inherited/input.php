<?php

declare(strict_types=1);

namespace App;

// `new static(…)` in a class that declares no constructor of its own refuses: `new Self(…)` calls the class's own
// `required` constructor, and declaring one changes the constructor PHP's reflection gives.
class Coupon extends Money
{
    public static function free(): static
    {
        return new static(0);
    }
}
