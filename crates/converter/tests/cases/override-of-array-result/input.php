<?php

declare(strict_types=1);

namespace App;

// An override of a plain PHP method declared `array` returns a `Map`, which keeps the plain PHP method's `array` in
// the PHP PHP# compiles to, so PHP links it (spec section 12).
final class SalesReport extends Report
{
    /** @return array<string, int> */
    public function totals(): array
    {
        return ['north' => 3];
    }
}
