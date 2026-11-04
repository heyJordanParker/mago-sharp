<?php

declare(strict_types=1);

namespace App;

// A `List`'s index write replaces an element, where PHP's adds one, so it waits for a later slice (spec section 12).
final class Slots
{
    /** @var list<string> */
    private array $names = [];

    public function put(int $at, string $name): void
    {
        $this->names[$at] = $name;
    }
}
