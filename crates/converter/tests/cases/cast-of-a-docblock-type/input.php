<?php

declare(strict_types=1);

namespace App;

// A type only PHPDoc gives is no proof of the value at runtime: `$row->quantity` may hold `"5"`, so `(int)` of it stays,
// and PHP#'s `(int)` converts it as PHP's does (spec section 24).
final class Totals
{
    public function quantity(Row $row): int
    {
        return (int) $row->quantity + 1;
    }
}
