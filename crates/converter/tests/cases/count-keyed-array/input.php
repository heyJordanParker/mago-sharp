<?php

declare(strict_types=1);

namespace App;

// PHP's `count` of an array with string keys is the `Map`'s `count()`, which the engine reads in constant time
// (spec section 12).
final class Fields
{
    /** @param array<string, string> $input */
    public function size(array $input): int
    {
        return count($input);
    }
}
