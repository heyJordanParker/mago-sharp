<?php

declare(strict_types=1);

namespace App;

// A read of a `Map` whose values are never null, at a key it may lack, throws where PHP warns on the missing key, a
// warning being PHP's own signal of a bug, and gives the value PHP gives anywhere else (spec section 12).
final class Sizes
{
    /** @var array<string, int> */
    private array $sizes = ['small' => 1];

    public function small(): int
    {
        return $this->sizes['small'];
    }

    public function bigger(string $name): int
    {
        return $this->sizes[$name] + 1;
    }

    // An array keyed by `int` that is no `list`, as `array_filter` leaves one, may hold keys with gaps, so it is a
    // `Map`, which reads key 0 as PHP does, where a `List` would read the first element.
    /** @param array<int, string> $names */
    public function first(array $names): string
    {
        return $names[0];
    }
}
