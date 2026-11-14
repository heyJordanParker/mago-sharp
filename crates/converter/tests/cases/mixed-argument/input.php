<?php

declare(strict_types=1);

namespace App;

// An argument of no known type is checked as its parameter's type, which PHP checks where the argument lands and
// throws a `TypeError` for any other value (spec section 21).
final class Greeter
{
    public function greet(mixed $name): string
    {
        return $this->hello($name);
    }

    public function welcome($guest, int $times): string
    {
        return $this->visit($guest, $times);
    }

    private function hello(string $name): string
    {
        return $name;
    }

    private function visit(Greeter $guest, int $times): string
    {
        return 'welcome';
    }
}
