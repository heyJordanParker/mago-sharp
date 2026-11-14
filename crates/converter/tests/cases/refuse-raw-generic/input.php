<?php

declare(strict_types=1);

namespace App;

// PHP takes a generic class with no type arguments as any of its kind, and a PHP# type names them, whose bounds hold
// each of its kind only for a covariant type parameter (spec section 11).
final class Shelf
{
    public function keep(Box $box): void
    {
    }

    /** @param Box<int> $box */
    public function count(Box $box): void
    {
    }
}
