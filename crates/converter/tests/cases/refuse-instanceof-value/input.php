<?php

declare(strict_types=1);

namespace App;

// `instanceof` of a class value and of `static` refuse: `is` tests a type the code names, and `Self` names the class
// PHP called only as a return type.
class Registry
{
    /** @param class-string $class */
    public function holds(object $item, string $class): bool
    {
        return $item instanceof $class;
    }

    public function sameKind(object $other): bool
    {
        return $other instanceof static;
    }
}
