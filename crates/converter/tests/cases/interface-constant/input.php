<?php

declare(strict_types=1);

namespace App;

// An interface's constant writes no access modifier, as the interface's methods take none (spec section 22).
interface Priced
{
    public const string CURRENCY = 'USD';
}
