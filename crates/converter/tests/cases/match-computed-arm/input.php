<?php

declare(strict_types=1);

namespace App;

// A `match` arm on a value that is no constant is a test with `==`, which is PHP's `===` for two `int` values, in a
// chain of ternaries that ends in `default`, or in PHP's `UnhandledMatchError` for an `int` with none (spec section 21).
final class Limits
{
    public function at(int $code, int $limit): string
    {
        return match ($code) {
            $limit => 'at the limit',
            0 => 'none',
            default => 'under',
        };
    }

    public function exact(int $code, int $limit): string
    {
        return match ($code) {
            $limit => 'at the limit',
        };
    }
}
