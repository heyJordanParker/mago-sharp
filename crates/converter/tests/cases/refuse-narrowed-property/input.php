<?php

declare(strict_types=1);

namespace App;

// PHP narrows a property `instanceof` tests, and PHP# reads a property by its declared type, since it may change
// between the test and the read (spec section 21). A read whose declared type fits where it lands converts.
final class Canvas
{
    public ?Circle $circle {
        get => $this->shape instanceof Circle ? $this->shape : null;
    }

    public function __construct(private ?Shape $shape = null)
    {
    }

    public function any(): ?Shape
    {
        return $this->shape instanceof Circle ? $this->shape : null;
    }
}
