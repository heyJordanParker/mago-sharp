<?php

declare(strict_types=1);

namespace App;

// A later slice converts an abstract class and a `readonly` one (spec sections 13 and 22).
abstract readonly class Shape
{
    public function __construct(public int $sides)
    {
    }
}
