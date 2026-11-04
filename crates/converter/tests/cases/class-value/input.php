<?php

declare(strict_types=1);

namespace App;

// `X::class` is the class value `typeof(X)`, `static` is `Self`, and `self` is the class's name (spec section 25).
final class Registry
{
    /** @return class-string<Spec> */
    public function kind(): string
    {
        return Spec::class;
    }

    public function fresh(): self
    {
        return new self();
    }

    public function same(): static
    {
        return $this;
    }
}
