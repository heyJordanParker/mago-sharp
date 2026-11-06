<?php

declare(strict_types=1);

namespace App;

// A cast of a value that already has the cast's type changes nothing in PHP, so PHP# writes the value alone, as a PHP#
// cast converts between numbers only (spec section 24).
final class Same
{
    public function text(string $value): string
    {
        return (string) $value . '!';
    }

    public function whole(int $value): int
    {
        return (int) ($value + 1);
    }

    public function real(float $value): float
    {
        return (float) $value;
    }
}
