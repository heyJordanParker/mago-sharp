<?php

declare(strict_types=1);

namespace App;

// An interface property states `get`, and each class gives its own `set` (spec section 22).
interface Titled
{
    public string $title { get; }
}
