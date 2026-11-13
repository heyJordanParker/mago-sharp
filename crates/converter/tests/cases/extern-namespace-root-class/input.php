<?php

declare(strict_types=1);

namespace App;

// A library's class in PHP's root namespace is imported through the namespace its Composer package's name gives, which
// the line the conversion adds gives its folder (spec section 23).
final class Orders
{
    public function reference(\Legacy_Order $order): string
    {
        return $order->reference;
    }

    public function blank(): \Legacy_Order
    {
        return new \Legacy_Order();
    }
}
