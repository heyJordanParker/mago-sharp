<?php

declare(strict_types=1);

namespace App;

// PHP turns a string key of an integer's digits into the `int`, as `'42'` into 42, and a PHP# `Map` keyed by
// `int|string` keeps it a string, so a key that may be one reads and writes another entry (spec section 12).
final class Stock
{
    /** @param array<array-key, int> $counts */
    public function counted(array $counts, string $sku): int
    {
        $counts['42'] = 1;
        unset($counts[$sku]);

        return $counts[$sku] ?? 0;
    }
}
