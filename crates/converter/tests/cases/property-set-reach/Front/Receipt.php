<?php

declare(strict_types=1);

namespace App\Front;

use App\Shop\Cart;

// Code outside `App\Shop` reads the properties, so they stay public.
final class Receipt
{
    public function line(Cart $cart): string
    {
        $cart->add();

        return $cart->owner . $cart->note . $cart->count;
    }
}
