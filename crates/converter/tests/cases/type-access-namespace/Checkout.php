<?php

namespace App;

final class Checkout
{
    public function run(): int
    {
        return (new Basket())->total();
    }
}
