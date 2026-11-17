<?php

declare(strict_types=1);

namespace App;

// PHP reads `""` and `"0"` as false, so a string's truthiness is `name != "" && name != "0"`, and an application
// compares no string with `"0"`, a string fixed at compile time (spec sections 21 and 24). The file stays plain PHP.
final class Greeting
{
    public function named(string $name): string
    {
        return $name ? $name : 'guest';
    }
}
