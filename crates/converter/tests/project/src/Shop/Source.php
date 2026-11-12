<?php

declare(strict_types=1);

namespace Proof\Shop;

// An interface method's parameter with a default value, which a class implements.
interface Source
{
    /** @param array<string, int> $limits */
    public function limited(array $limits = []): int;

    public function read(array $options = []): int;
}
