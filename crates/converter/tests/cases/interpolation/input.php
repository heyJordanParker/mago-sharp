<?php

declare(strict_types=1);

namespace App;

// An interpolated string is a template, which shows an `int`, a `float` and a `string` as PHP does (spec section 18).
final class Greeter
{
    public function greet(string $name, int $count, float $share): string
    {
        $first = "Hi $name, you have {$count} items";
        $second = "Share {$share} of \$5";

        return $first . $second;
    }
}
