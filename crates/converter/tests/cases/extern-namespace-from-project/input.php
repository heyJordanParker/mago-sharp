<?php

declare(strict_types=1);

namespace App;

// A function in PHP's root namespace of a project folder with no class is reached through the first PSR-4 namespace
// the project's `composer.json` lists, as it names no package, which the line the conversion adds gives its folder
// (spec section 23).
final class Greeter
{
    public function greet(string $name): string
    {
        return shout_all($name);
    }
}
