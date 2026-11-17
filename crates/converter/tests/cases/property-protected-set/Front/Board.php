<?php

declare(strict_types=1);

namespace App\Front;

use App\Shop\Counter;

// Code outside `App\Shop` reads `count`, so it stays public.
final class Board
{
    public function shown(Counter $counter): int
    {
        return $counter->count;
    }
}
