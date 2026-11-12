<?php

declare(strict_types=1);

namespace App;

// A loop over the values of an object whose class implements `Traversable`, as a framework's collection or a
// generator, is PHP's own `foreach` over it in PHP# too (spec section 17).
final class Order
{
    public function total(Lines $lines): int
    {
        $total = 0;
        foreach ($lines as $line) {
            $total += $line->amount;
        }

        return $total;
    }
}
