<?php

declare(strict_types=1);

namespace Proof\Shop;

// Stays plain PHP: PHP turns the key `'42'` into the `int` 42, and a PHP# `Map` keeps it as written, so a keyed loop
// over a `string`-keyed array would find a key PHP's doesn't.
final class Ledger
{
    /** @param array<string, int> $prices */
    public function priceOf(array $prices, string $sku): int
    {
        $found = 0;
        foreach ($prices as $key => $price) {
            if ($key === $sku) {
                $found = $price;
            }
        }

        return $found;
    }
}
