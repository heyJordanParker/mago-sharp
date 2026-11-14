<?php

declare(strict_types=1);

namespace App\Tenant\Store\Entities;

use App\Tenant\Store\CartService;
use App\Tenant\Store\Entities\Lines\Line;

// `Cart` imports `App\Tenant\Store`, its parent namespace, which imports `Cart` back, and PHP# lets no two namespaces
// import each other. `Lines` imports nothing of this namespace, so its import is no cycle.
final class Cart
{
    public function total(CartService $service, Line $line): int
    {
        return $service->count() + $line->quantity();
    }
}
