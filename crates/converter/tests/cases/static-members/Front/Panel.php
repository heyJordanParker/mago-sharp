<?php

declare(strict_types=1);

namespace Front;

use App\Counter;

// Code outside `App` uses `Counter`'s members, so they stay public.
final class Panel
{
    public function shown(Counter $counter): string
    {
        return $counter->step() . $counter->label . ($counter->value + Counter::made() + Counter::LIMIT);
    }
}
