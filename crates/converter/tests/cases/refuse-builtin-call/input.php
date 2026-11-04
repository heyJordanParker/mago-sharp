<?php

declare(strict_types=1);

namespace App;

// A built-in that no standard library member runs with PHP's meaning keeps the file plain PHP, with the difference
// named, a type test of a type PHP# can't write does too, and `compact` and `extract` are removed (spec sections 2 and
// 21, decision 083).
final class Text
{
    public function clean(string $text): string
    {
        return trim($text);
    }

    public function lower(string $text): string
    {
        return mb_strtolower($text);
    }

    /** @return array<string, string> */
    public function pack(string $name): array
    {
        return compact('name');
    }

    public function sized(mixed $value): bool
    {
        return is_array($value);
    }
}
