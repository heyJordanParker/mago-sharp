<?php

declare(strict_types=1);

namespace App;

// PHP# writes an empty `List` `[]` and an empty `Map` `[:]`, so `[]` where either lands picks neither (spec section 12).
// A PHP# type holds one of them, and a plain PHP parameter may take both.
final class Either
{
    public function fill(Sink $sink): void
    {
        $sink->take([]);
    }
}
