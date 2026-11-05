<?php

declare(strict_types=1);

namespace App;

// `: super(…)` runs the parent's constructor before the body, so one the body runs after another statement has no
// PHP# form (spec section 9).
final class Checking extends Account
{
    public function __construct(string $owner)
    {
        $owner = $owner . '!';
        parent::__construct($owner);
    }
}
