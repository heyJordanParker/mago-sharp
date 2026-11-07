<?php

declare(strict_types=1);

namespace App;

// PHP's `iterable<V>` keys by any `array-key`, which says no more than a PHP# `Iterable<V>`. A PHP# `Iterable<V>`
// parameter takes a new sequence of the values it is given, numbered from 0, so a loop over its values alone reads
// what PHP's reads (spec section 12).
final class Joiner
{
    /** @param iterable<string> $items */
    public function joined(iterable $items): string
    {
        $shown = '';
        foreach ($items as $item) {
            $shown .= $item;
        }

        return $shown;
    }

    /** @param iterable<array-key, int> $counts */
    public function total(iterable $counts): int
    {
        $total = 0;
        foreach ($counts as $count) {
            $total += $count;
        }

        return $total;
    }
}
