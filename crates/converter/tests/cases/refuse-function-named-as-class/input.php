<?php

declare(strict_types=1);

namespace App;

// A library's root function is reached through the namespace its `extern namespace` line gives it (spec section 23),
// and where a class of that namespace takes the function's name, `Text.str` names the class: PHP finds a class in any
// case.
final class Labels
{
    public function label(string $key): string
    {
        return str($key);
    }
}
