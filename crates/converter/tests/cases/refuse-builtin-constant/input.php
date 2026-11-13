<?php

declare(strict_types=1);

namespace App;

// PHP's own constants reach PHP# only through a standard library constant whose value is one, which `PHP_EOL` has
// none of (decision 083), and a root function with no class beside it, in no Composer package, has no namespace to be
// reached through (spec section 23).
final class Lines
{
    public function end(): string
    {
        return PHP_EOL;
    }

    public function shouted(string $text): string
    {
        return shout($text);
    }
}
