<?php

declare(strict_types=1);

namespace App;

// PHP's `__toString()` is PHP#'s `string toString()`, and `clone $x` is `x with { }`, a copy with no property set
// (spec sections 10 and 18).
class Sku
{
    public function __construct(public string $code)
    {
    }

    public function __toString(): string
    {
        return 'SKU ' . $this->code;
    }

    public function copy(): Sku
    {
        return clone $this;
    }
}
