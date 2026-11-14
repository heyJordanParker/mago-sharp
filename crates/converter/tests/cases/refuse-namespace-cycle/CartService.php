<?php

declare(strict_types=1);

namespace App\Tenant\Store;

use App\Tenant\Store\Entities\Cart;

final class CartService
{
    public function count(): int
    {
        return 1;
    }

    public function open(): Cart
    {
        return new Cart();
    }
}
