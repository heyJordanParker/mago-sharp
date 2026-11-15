<?php

declare(strict_types=1);

namespace Sale;

use App\Cart;

// A subclass outside `App`, which replaces `total` and calls it.
final class BigCart extends Cart
{
    public function total(): int
    {
        return parent::total() * 2;
    }
}
