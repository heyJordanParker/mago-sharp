<?php

declare(strict_types=1);

namespace Web;

use App\Cart;

// Code outside `App`.
final class Page
{
    public function render(Cart $cart): string
    {
        return $cart->label();
    }
}
