<?php

declare(strict_types=1);

namespace App;

use DateTimeImmutable;

// PHP# names a PHP built-in class outside `Builtins.sharp` only in the standard library (spec section 23).
final class Clock
{
    public function now(): \DateTimeImmutable
    {
        return new \DateTimeImmutable();
    }
}
