<?php

declare(strict_types=1);

namespace App;

// A `Map` read or written at a string fixed at compile time names a field of a fixed shape, which an application
// declares as a struct (spec section 24). The file stays plain PHP.
final class Sizes
{
    /** @param array<string, int> $sizes */
    public function small(array $sizes): int
    {
        $sizes['large'] = 3;

        return $sizes['small'];
    }
}
