<?php

declare(strict_types=1);

namespace App;

// PHP's own constant, read through the standard library constant whose value it is (spec section 23).
final class Limits
{
    public function highest(): int
    {
        return PHP_INT_MAX;
    }

    public function lowest(): int
    {
        return \PHP_INT_MIN;
    }
}
