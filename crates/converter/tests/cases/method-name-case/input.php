<?php

declare(strict_types=1);

namespace App;

// PHP finds a method whatever case a call writes, and PHP# by the case its class declares (spec section 25).
final class Greeter
{
    public function greetAll(): string
    {
        return $this->GREET('all');
    }

    public function greet(string $name): string
    {
        return 'hi ' . $name;
    }
}
