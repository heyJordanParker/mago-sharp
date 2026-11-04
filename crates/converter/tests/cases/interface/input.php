<?php

declare(strict_types=1);

namespace App;

// An interface's members take no access modifier, and its parents follow `:` (spec sections 5 and 22).
interface Priced extends Named
{
    public function price(int $quantity): float;
}
