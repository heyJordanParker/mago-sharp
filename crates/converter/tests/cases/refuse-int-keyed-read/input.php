<?php

declare(strict_types=1);

namespace App;

// An array keyed by `int` that is no `list`, as `array_filter` leaves one, holds keys with gaps. PHP reads key 3, and
// a PHP# `List` would read the fourth element, a different value with no failure, so the read is a `Map` read at a key
// it may lack, which refuses (spec section 12).
final class People
{
    /** @param array<int, string> $names */
    public function third(array $names): string
    {
        return $names[3];
    }
}
