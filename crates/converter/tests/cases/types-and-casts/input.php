<?php

declare(strict_types=1);

namespace App;

// `mixed` is `Any?`, `?T` is `T?` and `object` is `Object`. `(int)`, `(float)` and `(string)` stay, and PHP's aliases
// become them (spec section 24).
final class Convert
{
    public function any(mixed $value): mixed
    {
        return $value;
    }

    public function maybe(?int $value): ?int
    {
        return $value;
    }

    public function thing(object $value): object
    {
        return $value;
    }

    public function whole(float $value): int
    {
        return (integer) $value + (int) $value;
    }

    public function real(int $value): float
    {
        return (double) $value * 1.5;
    }

    public function text(int $value): string
    {
        return (string) $value;
    }
}
