<?php

declare(strict_types=1);

namespace App;

// A PHP built-in reaches PHP# through the standard library, which a later slice writes, and `compact` and `extract`
// are removed (spec sections 2 and 8).
final class Text
{
    public function clean(string $text): string
    {
        return trim($text);
    }

    /** @param list<string> $items */
    public function size(array $items): int
    {
        return count($items);
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
