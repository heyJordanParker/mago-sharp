<?php

declare(strict_types=1);

namespace App;

use App\Errors\OutOfRangeException;

// A read of a `Map` at a key PHP may turn into an `int` reads another entry, and so refuses, beside a `use` of a class
// named like the root `OutOfRangeException`, which the conversion imports under another name (spec section 12).
final class Ledger
{
    /** @param array<array-key, int> $totals */
    public function total(array $totals, string $name): int
    {
        return $totals[$name];
    }

    /** @param array<string, int> $totals */
    public function named(array $totals): int
    {
        return $totals['rent'];
    }

    public function failure(): OutOfRangeException
    {
        return new OutOfRangeException();
    }
}
