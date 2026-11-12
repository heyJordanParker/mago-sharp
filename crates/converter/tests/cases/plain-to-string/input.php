<?php

declare(strict_types=1);

namespace App;

// PHP's `(string)`, `.` and interpolation of an object call its class's `__toString`, which PHP# reads as a plain PHP
// class's `toString()` (spec section 18).
final class Mailer
{
    public function address(Email $to): string
    {
        return (string) $to->self();
    }

    public function line(Email $to): string
    {
        return 'to ' . $to . " ({$to})";
    }
}
