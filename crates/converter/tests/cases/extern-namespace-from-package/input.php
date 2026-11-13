<?php

declare(strict_types=1);

namespace App;

// A function in PHP's root namespace of a library with no class is reached through the namespace its Composer
// package's name gives, `acme/string-helpers` giving `Acme\StringHelpers`, which the line the conversion adds gives
// its folder (spec section 23).
final class Greeter
{
    public function greet(string $name): string
    {
        return str_shout($name);
    }
}
