<?php

declare(strict_types=1);

namespace Web;

use App\Shelf;

// Code outside `App` calls `Shelf`'s methods, so they stay public.
final class Page
{
    public function render(Shelf $shelf): string
    {
        return (string) $shelf->label() . (string) $shelf->counted();
    }
}
