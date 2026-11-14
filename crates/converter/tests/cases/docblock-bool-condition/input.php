<?php

declare(strict_types=1);

namespace App;

// A `bool` condition compiles to a test of the value itself, which reads PHP's truthiness of whatever the value holds
// at runtime, so a value only PHPDoc types `bool`, which may hold `1`, converts in `if`, `!`, `&&`, `||` and `?:`
// (spec section 21).
final class Status
{
    public function shown(Row $row): string
    {
        if ($row->active) {
            return 'on';
        }

        return 'off';
    }

    public function hidden(Row $row): bool
    {
        $active = $row->active;

        return !$active;
    }

    public function both(Row $row, bool $first): bool
    {
        return $first && $row->active || !$row->active;
    }

    public function either(Row $row, bool $first): bool
    {
        $active = $row->active;

        return $active ?: $first;
    }

    // A write after the test that may not hold a `bool` changes nothing, since the test reads any value.
    /** @param list<Row> $rows */
    public function seen(array $rows, bool $first): int
    {
        $seen = $first;
        $count = 0;
        foreach ($rows as $row) {
            if ($seen) {
                $count = $count + 1;
            }
            $seen = $row->active;
        }

        return $count;
    }
}
