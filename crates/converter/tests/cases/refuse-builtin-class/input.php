<?php

declare(strict_types=1);

namespace App;

use DateTimeImmutable;

// PHP# names a PHP built-in class outside `Builtins.sharp` only in the standard library (spec section 23), so the file
// refuses where it names one, as a type and after `new`, and its `use` line goes.
final class Clock
{
    public function now(): \DateTimeImmutable
    {
        return new \DateTimeImmutable();
    }

    public function year(): string
    {
        return (new DateTimeImmutable())->format('Y');
    }
}
