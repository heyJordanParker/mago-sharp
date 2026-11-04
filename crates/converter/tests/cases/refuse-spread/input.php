<?php

declare(strict_types=1);

namespace App;

// Only a `List` spreads into a call, and only into a `...` parameter. A `Map` spread keeps an `int` key PHP renumbers
// (spec sections 7 and 12).
final class Spreads
{
    public function pair(int $first, int $second): int
    {
        return $first + $second;
    }

    /**
     * @param list<int> $numbers
     * @param array<int, string> $byId
     *
     * @return array<int, string>
     */
    public function run(array $numbers, array $byId): array
    {
        $this->pair(...$numbers);

        return [...$byId];
    }
}
