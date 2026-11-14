<?php

declare(strict_types=1);

namespace App\Shop\Checkout;

use App\Shop\StoreService;
use App\Support\Container;

// A plain PHP call that gives a service a module binds, which PHP# takes only through a constructor or a parameter
// (spec section 32).
final class Checkout
{
    public function count(): int
    {
        return Container::make(StoreService::class)->count();
    }

    public function counted(StoreService $store): int
    {
        return $store->count();
    }
}
