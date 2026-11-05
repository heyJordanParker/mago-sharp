<?php

declare(strict_types=1);

namespace App;

// A key a `Map` may lack is read behind `??`, as in PHP (spec section 12).
final class Sizes
{
    /** @var array<string, int> */
    private array $sizes = ['small' => 1];

    public function size(string $name): int
    {
        return $this->sizes[$name] ?? 0;
    }
}
