<?php

declare(strict_types=1);

namespace App;

// PHP# casts convert between numbers, and `(string)` writes a number as text (spec section 24). PHP's casts of any other
// value, an array, a `bool` or a string it reads as a number, have no PHP# form.
final class Casts
{
    /** @param string|list<string> $payload */
    public function text(string|array $payload): string
    {
        return (string) $payload;
    }

    public function flag(bool $flag): string
    {
        return (string) $flag;
    }

    public function count(string $digits): int
    {
        return (int) $digits;
    }
}
