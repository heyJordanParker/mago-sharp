<?php

declare(strict_types=1);

namespace App;

// A PHP# local is declared where it is first assigned and lives in its block (spec sections 2 and 3), so a local
// that starts with no type, starts inside another expression, or is a loop's variable read after the loop keeps the
// file plain PHP.
final class Locals
{
    /** @return list<int> */
    public function empty(): array
    {
        $items = [];

        return $items;
    }

    public function inside(?string $text): string
    {
        if (($found = $text) !== null) {
            return $found;
        }

        return '';
    }

    /** @param list<int> $items */
    public function after(array $items): int
    {
        foreach ($items as $item) {
        }

        return $item;
    }
}
