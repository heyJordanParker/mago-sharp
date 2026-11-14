<?php

declare(strict_types=1);

namespace App\Shop;

final class Checkout
{
    public function mark(Cart $cart): void
    {
        $cart->note = 'gift';
    }
}
