<?php

declare(strict_types=1);

namespace App;

// A subclass's constructor runs its parent's first, as `: super(…)` with the arguments of the `parent::__construct(…)`
// its body starts with, which leaves the body (spec section 9).
final class Savings extends Account
{
    private int $rate;

    public function __construct(string $owner, int $rate)
    {
        parent::__construct($owner);
        $this->rate = $rate;
    }

    public function interest(): int
    {
        return $this->rate;
    }
}
