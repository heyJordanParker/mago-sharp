<?php

declare(strict_types=1);

namespace App;

// `instanceof self` tests the class that declares the method, by its name (spec sections 21 and 25).
class Money
{
    public function sameKind(object $other): bool
    {
        return $other instanceof self;
    }
}
