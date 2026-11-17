<?php

declare(strict_types=1);

namespace Laps;

use App\Counter;

// A subclass outside `App` alone reads `steps`, so it is protected.
final class Lap extends Counter
{
    public function laps(): int
    {
        return $this->steps;
    }
}
