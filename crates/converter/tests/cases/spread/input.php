<?php

declare(strict_types=1);

namespace App;

// A `List` spreads into a `...` parameter and into a literal (spec sections 7 and 12).
final class Prices
{
    public function sum(int ...$amounts): int
    {
        $total = 0;
        foreach ($amounts as $amount) {
            $total += $amount;
        }

        return $total;
    }

    /**
     * @param list<int> $first
     * @param list<int> $second
     *
     * @return list<int>
     */
    public function join(array $first, array $second): array
    {
        $all = [...$first, ...$second];
        $count = $this->sum(...$all);

        return [...$all, $count];
    }

    // Each argument past the first lands in the `...` parameter too.
    public function pair(): int
    {
        return $this->sum(1, 2);
    }
}
