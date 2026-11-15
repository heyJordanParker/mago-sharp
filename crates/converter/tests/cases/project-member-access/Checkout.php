<?php

declare(strict_types=1);

namespace App;

// Code of `Cart`'s namespace.
final class Checkout
{
    public function run(Cart $cart): string
    {
        $cart->add();

        return $cart->owner;
    }
}
