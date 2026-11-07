<?php

declare(strict_types=1);

namespace App;

// A class named `Module` converts into `Module.sharp`, which holds only its folder's module (spec section 32).
abstract class Module
{
    abstract public function boot(): void;
}
