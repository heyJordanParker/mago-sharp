<?php

declare(strict_types=1);

namespace App;

// A library's root function is reached through the namespace of the classes beside it (spec section 23), and where
// one of those classes takes the function's name, `Text.str` names the class: PHP finds a class in any case.
final class Labels
{
    public function label(string $key): string
    {
        return str($key);
    }
}
