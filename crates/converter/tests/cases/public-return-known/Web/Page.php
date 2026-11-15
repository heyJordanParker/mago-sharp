<?php

declare(strict_types=1);

namespace Web;

use App\Store;

// Code outside `App` calls `Store`'s methods, so they stay public.
final class Page
{
    public function render(Store $store): string
    {
        return $store->label() . (string) $store->counted(true);
    }
}
