<?php

declare(strict_types=1);

namespace App;

// A `List` spreads into a `...` parameter and into a literal, and a `Map` keyed by `string` into a literal (spec
// sections 7 and 12).
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
     * @param array<string, int> $named
     * @param array<string, int> $more
     *
     * @return array<string, int>
     */
    public function join(array $first, array $second, array $named, array $more): array
    {
        $all = [...$first, ...$second];
        $count = $this->sum(...$all);

        return [...$named, ...$more, 'count' => $count];
    }
}
