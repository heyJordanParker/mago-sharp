<?php

declare(strict_types=1);

namespace App\Front;

use App\Shop\Store;

// Code outside `App\Shop` uses `Store`'s public members, so they stay public.
final class Page
{
    public function render(Store $store): int
    {
        $store->take([]);
        $payload = $store->checkoutPayload();

        return $store->counted(['a' => 1]) + (is_object($store->handle) ? 1 : 0) + ($payload === null ? 0 : 1);
    }
}
