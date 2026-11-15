<?php

declare(strict_types=1);

namespace App;

// PHP's `/` of two `int`s gives an `int` where the quotient is whole and a `float` where it is not, and PHP#'s
// truncates (spec section 24). A `float` place divides as floats, and a place of both tells the two apart by `%`.
final class Invoice
{
    public function tax(int $total, int $rate): float
    {
        return $total * $rate / 100;
    }

    public function ratio(int $total, int $parts): int|float
    {
        return $total / $parts;
    }

    public function half(int $count): int|float
    {
        $half = $count / 2;

        return $half;
    }

    public function share(int $total, int $parts): int
    {
        return intdiv($total, $parts);
    }

    public function pairs(int $total, int $parts): int
    {
        return 2 * intdiv($total, $parts + 1);
    }
}
