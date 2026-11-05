<?php

declare(strict_types=1);

namespace App;

// A member named as one the class inherits, a property as a method or a method as a property, clashes as one the class
// declares does (spec section 14).
final class Child extends Base
{
    public string $label = 'child';

    public function title(): string
    {
        return 'child';
    }
}
