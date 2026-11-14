<?php

declare(strict_types=1);

namespace App;

// A `match` on an `int` with no `default` arm gains the one that throws PHP's `UnhandledMatchError`, whose message holds
// the `int` (spec section 21).
final class Codes
{
    public function name(int $code): string
    {
        return match ($code) {
            1 => 'one',
            2 => 'two',
        };
    }

    public function short(int $code): string
    {
        return match ($code) { 1 => 'one', 2 => 'two' };
    }
}
