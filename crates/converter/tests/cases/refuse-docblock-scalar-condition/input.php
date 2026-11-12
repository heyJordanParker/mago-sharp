<?php

declare(strict_types=1);

namespace App;

// A condition reads a scalar by its type, which a value only PHPDoc types may not hold at runtime: `1` is no `bool`
// to PHP#, and `"0"` is no `int`. So `match (true)`, `(bool)`, `if` and `!` refuse such a value, and a local written
// one, where PHP reads it by its own truthiness (spec section 21).
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

    public function hidden(Row $row): bool
    {
        $active = $row->active;

        return !$active;
    }

    // A write after the test that may not hold a `bool` refuses the test, which a loop reaches again.
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
