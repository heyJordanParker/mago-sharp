<?php

declare(strict_types=1);

namespace App;

// An interface's members are public, and a `set` reaches its own namespace at most, so an interface property states
// `get` or `init` and each class gives its own `set` (spec section 22).
interface Named
{
    public string $name { get; set; }

    public string $title { get; }
}
