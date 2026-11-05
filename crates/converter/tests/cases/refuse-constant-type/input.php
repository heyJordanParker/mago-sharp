<?php

declare(strict_types=1);

namespace App;

// The lowering writes a class constant's type into its PHP, and PHP makes a subclass's constant of the same name take
// a compatible type. A constant PHP declares with no type takes the type of its value, so it converts only where no
// subclass declares it again: in a `final` class, or as a `private` constant.
class Limits
{
    public const LOW = 3;

    protected const HIGH = 9;

    private const NAME = 'limits';

    public const int TYPED = 5;

    public function name(): string
    {
        return self::NAME;
    }
}
