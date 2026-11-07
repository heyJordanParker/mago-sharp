<?php

declare(strict_types=1);

namespace App;

// PHP turns a numeric string key into an `int`, as `'42'` into 42, and a PHP# `Map` keeps the key as written, so a
// keyed loop or a spread of a `Map` keyed by anything but `int` refuses (spec section 12), and `array_keys` and `key`
// have no PHP# form. An `int`-keyed loop converts.
final class Prices
{
    /** @param array<string, int> $prices */
    public function shown(array $prices): string
    {
        $shown = '';
        foreach ($prices as $sku => $price) {
            $shown .= $sku . '=' . $price . ' ';
        }

        return $shown;
    }

    /**
     * @param array<string, int> $prices
     * @return array<string, int>
     */
    public function copied(array $prices): array
    {
        return [...$prices];
    }

    /**
     * @param array<string, int> $prices
     * @return list<string>
     */
    public function skus(array $prices): array
    {
        return array_keys($prices);
    }

    /** @param array<string, int> $prices */
    public function current(array $prices): ?string
    {
        return key($prices);
    }

    /** @param array<int, int> $counts */
    public function numbered(array $counts): int
    {
        $total = 0;
        foreach ($counts as $id => $count) {
            $total += $id * $count;
        }

        return $total;
    }
}
