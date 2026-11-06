<?php

declare(strict_types=1);

namespace App;

// PHP#'s `(int)` and `(float)` run PHP's own conversion at runtime, so a cast of a number stays, whatever type the
// analysis gives it. A `(string)` of a string leaves only where a native declaration or a literal makes the value one at
// runtime, as a PHP# cast converts a number (spec section 24).
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
