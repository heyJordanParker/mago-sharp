<?php

declare(strict_types=1);

namespace App;

// A local that starts as `[]` or `null` takes its type from the declared types of the values written to it later:
// `List<V>` when the body only appends to it, `Map<K, V>` when it only writes it by key, and `T?` after `null`. PHP#
// declares it first in the body (spec sections 2, 3 and 12, decision 2).
final class Basket
{
    /** @param list<string> $names */
    public function upper(array $names): int
    {
        $found = [];
        foreach ($names as $name) {
            $found[] = $name;
        }

        return count($found);
    }

    /** @return array<string, int> */
    public function prices(string $sku, int $price): array
    {
        $prices = [];
        $prices[$sku] = $price;

        return $prices;
    }

    public function last(bool $wide, string $name): string
    {
        $chosen = null;
        if ($wide) {
            $chosen = $name;
        }

        return $chosen ?? '';
    }

    // One first assigned inside a block is declared first in the body, where each pass of the loop assigns it.
    /** @param list<string> $names */
    public function longest(array $names): int
    {
        $most = 0;
        foreach ($names as $name) {
            $letters = [];
            $letters[] = $name;
            $most = count($letters);
        }

        return $most;
    }
}
