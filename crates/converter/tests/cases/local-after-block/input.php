<?php

declare(strict_types=1);

namespace App;

// A local assigned in a block and read after it is declared first in its method's body, as `T? name = null;`, and
// each assignment of it writes it (spec section 3).
final class Pricing
{
    public function price(bool $member): int
    {
        if ($member) {
            $price = 10;
        } else {
            $price = 20;
        }

        return $price;
    }

    public function label(int $count): string
    {
        $suffix = '';
        if ($count > 1) {
            $plural = 's';
            $suffix = $plural;
        }

        return 'item' . $suffix;
    }

    // A PHP local lives for the whole method, so one a loop's pass assigns keeps its value into the next pass, and
    // its declaration goes first in the method's body.
    /** @param list<int> $values */
    public function carried(array $values): int
    {
        $sum = 0;
        foreach ($values as $value) {
            if ($value > 0) {
                $last = $value;
            }
            $sum += $last ?? 0;
        }

        return $sum;
    }
}
