<?php

declare(strict_types=1);

namespace App;

// PHP's truthiness of a value that may be null is a test of several parts, and so is `?:`, which tests a value and
// gives it. Each reads a variable again. A property never narrows, since each read may give another value, so an `if`
// binds a property it tests that its body reads, and `?:` reads one once, through `match` (spec section 21).
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
