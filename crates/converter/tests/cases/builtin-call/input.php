<?php

declare(strict_types=1);

namespace App;

// A PHP built-in becomes the standard library member whose body runs it with PHP's meaning: an extension member on
// its receiver, or a static member through its class (decision 4).
final class Words
{
    /** @var list<string> */
    private array $names = [];

    public function size(): int
    {
        return count($this->names);
    }

    public function joined(): string
    {
        return implode(', ', $this->names);
    }

    public function prefixed(string $text): bool
    {
        return str_starts_with($text, 'a');
    }

    public function digest(string $data): string
    {
        return md5($data);
    }

    public function rounded(float $value): float
    {
        return round($value, 2);
    }
}
