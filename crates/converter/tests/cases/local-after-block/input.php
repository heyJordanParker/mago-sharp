<?php

declare(strict_types=1);

namespace App;

// A local assigned in a block and read after it is declared before the block, as `T? name = null;`, and each
// assignment of it writes it (spec section 3).
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
}
