<?php

declare(strict_types=1);

namespace App;

// A loop into a pattern by position has no PHP# form, since a plain PHP array is no tuple, and a loop into a pattern
// of keys reads keys a row may lack, as a `Map` read does.
final class Pairs
{
    /** @param list<list<string>> $pairs */
    public function firsts(array $pairs): string
    {
        $text = '';
        foreach ($pairs as [$first, $second]) {
            $text .= $first . $second;
        }

        return $text;
    }

    /** @param list<array<string, string>> $rows */
    public function names(array $rows): string
    {
        $text = '';
        foreach ($rows as ['name' => $name]) {
            $text .= $name;
        }

        return $text;
    }
}
