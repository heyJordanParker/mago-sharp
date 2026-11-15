<?php

declare(strict_types=1);

namespace App;

use App\Errors\OutOfRangeException;

// A `use` that takes the name of a root class of `Builtins.sharp`, which a `.sharp` file names bare, imports its class
// under the name of its namespace before it, so the file still reaches the root `OutOfRangeException` a `Map` read
// throws where PHP warns (spec sections 12 and 23).
final class Ledger
{
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
