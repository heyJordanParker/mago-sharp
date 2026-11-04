<?php

declare(strict_types=1);

namespace App;

// A PHP# local is declared where it is first assigned and lives in its block (spec sections 2 and 3), so a local
// that starts with no type, starts inside another expression, or lives on after its block keeps the file plain PHP.
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

    public function after(bool $flag): int
    {
        if ($flag) {
            $value = 1;
        } else {
            $value = 2;
        }

        return $value;
    }
}
