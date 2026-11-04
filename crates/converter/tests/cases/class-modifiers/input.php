<?php

declare(strict_types=1);

namespace App;

// An abstract class keeps `abstract`, and a `readonly` class is each of its properties `{ get; }` (spec sections 13
// and 22).
abstract readonly class Shape
{
    public function __construct(public int $sides)
    {
    }
}
