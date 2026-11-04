<?php

declare(strict_types=1);

namespace App;

// A later slice converts an interface (spec section 22).
interface Named
{
    public function name(): string;
}
