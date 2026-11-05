<?php

declare(strict_types=1);

namespace App;

// An abstract class keeps `abstract`, and its abstract methods take no `virtual`, since a subclass replaces them by
// definition (spec section 22).
abstract class Shape
{
    abstract public function area(): float;

    public function describe(): string
    {
        return 'area ' . $this->area();
    }
}
