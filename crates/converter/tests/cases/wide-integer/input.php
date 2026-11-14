<?php

declare(strict_types=1);

namespace App;

// PHP reads an integer literal past `PHP_INT_MAX` as the `float` nearest its value: a decimal one as a float literal of
// its digits, and a hex one digit by digit (spec section 24).
final class Big
{
    public function huge(): float
    {
        return 18446744073709551616;
    }

    public function hex(): float
    {
        return 0x1_0000_0000_0000_0000;
    }
}
