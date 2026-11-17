<?php

declare(strict_types=1);

namespace App;

// PHP's truthiness of a value that may be null is a test of several parts, and so is `?:`, which tests a value and
// gives it. Each reads the value again, which gives what PHP's one read gives for a variable, and for a property its
// class declares with no `get` hook and no `__get` (spec section 21).
final class Customer
{
    private ?int $visits = null;

    public function __construct(private ?int $points)
    {
    }

    public function score(?int $bonus): ?int
    {
        if ($this->visits) {
            return $this->visits;
        }

        return ($bonus ?: 1) + ($this->points ?: 0);
    }
}
