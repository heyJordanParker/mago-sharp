<?php

declare(strict_types=1);

namespace Proof\Shop;

// `new static(…)`, which makes an object of the class PHP called, a plain PHP subclass's included, through the
// constructor PHP# marks `required`.
class Coupon
{
    public function __construct(public readonly int $cents)
    {
    }

    public function doubled(): static
    {
        return new static($this->cents * 2);
    }
}
