<?php

namespace Web;

use App\Basket;

final class Page
{
    public function render(): int
    {
        return (new Basket())->total();
    }
}
