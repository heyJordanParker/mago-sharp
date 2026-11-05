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

    // The local's type joins those of the values assigned to it, which a declaration or a literal gives.
    public function chosen(bool $wide): int
    {
        if ($wide) {
            $size = $this->price($wide);
        } else {
            $size = 1;
        }

        return $size;
    }

    // A `@var` tag on any assignment of the local declares its type, which every value assigned to it has.
    public function tagged(bool $wide, int $count): int
    {
        if ($wide) {
            $size = $count * 2;
        } else {
            /** @var int $size */
            $size = 1;
        }

        return $size;
    }
}
