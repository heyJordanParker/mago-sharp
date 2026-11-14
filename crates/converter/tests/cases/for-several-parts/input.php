<?php

declare(strict_types=1);

namespace App;

// A `for` keeps the commas of several initializations, conditions and increments, where its initializations assign
// locals declared before it (spec section 17).
final class Pairs
{
    public function sum(int $count): int
    {
        $total = 0;
        $index = 0;
        $last = $count;
        for ($index = 0, $last = $count - 1; $index < $last; $index++, $last--) {
            $total += $index;
        }

        return $total + $last;
    }
}
