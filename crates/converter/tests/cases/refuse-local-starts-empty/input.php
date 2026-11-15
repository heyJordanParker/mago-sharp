<?php

declare(strict_types=1);

namespace App;

// A local that starts as `[]` and is both appended to and written by key, or that starts as `[]` or `null` and is
// written a value no declaration gives the type of, keeps the file plain PHP (decision 2).
final class Mixed
{
    public function both(string $key): int
    {
        $values = [];
        $values[] = 1;
        $values[$key] = 2;

        return count($values);
    }

    /** @param list<int> $items */
    public function guessed(array $items): int
    {
        $values = [];
        $values[] = $items[0];

        return count($values);
    }

    /** @param list<int> $items */
    public function nulled(bool $wide, array $items): int
    {
        $size = null;
        if ($wide) {
            $size = $items[0];
        }

        return $size ?? 0;
    }
}
