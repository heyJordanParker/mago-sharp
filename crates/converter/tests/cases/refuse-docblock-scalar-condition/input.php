<?php

declare(strict_types=1);

namespace App;

// A condition reads a number or a string by its type, which a value only PHPDoc types may not hold at runtime:
// `"0"` is no `int`. So `(bool)` and `if` refuse such a value, where PHP reads it by its own truthiness, and
// `match (true)` refuses a `bool` that may hold `1`, which PHP compares with `true` by `===` (spec section 21).
final class Status
{
    public function label(Row $row): string
    {
        return match (true) {
            $row->active => 'on',
            default => 'off',
        };
    }

    public function counted(Row $row): bool
    {
        return (bool) $row->count;
    }

    public function shown(Row $row): string
    {
        if ($row->count) {
            return 'some';
        }

        return 'none';
    }

    // A write after the test that may not hold an `int` refuses the test, which a loop reaches again.
    /** @param list<Row> $rows */
    public function seen(array $rows, int $first): int
    {
        $seen = $first;
        $count = 0;
        foreach ($rows as $row) {
            if ($seen) {
                $count = $count + 1;
            }
            $seen = $row->count;
        }

        return $count;
    }
}
