<?php

declare(strict_types=1);

namespace App;

// PHP's own constants reach PHP# through the standard library, which a later slice writes, and a root function with
// no class beside it has no namespace to be reached through (spec section 23).
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
