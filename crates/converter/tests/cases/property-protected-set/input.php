<?php

declare(strict_types=1);

namespace App\Shop;

// A `set` reaches its own namespace at most, and `protected set` is a compile error, so PHP's `protected(set)` is a
// `set` where every write is inside the namespace, as the subclass's here is (spec section 6.1).
class Counter
{
    public protected(set) int $count = 0;
}
