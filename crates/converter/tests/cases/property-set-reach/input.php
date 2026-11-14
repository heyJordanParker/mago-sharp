<?php

declare(strict_types=1);

namespace App\Shop;

// A PHP# property's `set` reaches its namespace at most, and is `private` where the class alone sets the property
// (spec section 6).
final class Cart
{
    public int $count = 0;

    public string $note = '';

    public function __construct(public string $owner)
    {
    }

    public function add(): void
    {
        $this->count++;
    }
}
