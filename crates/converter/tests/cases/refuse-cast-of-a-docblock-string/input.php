<?php

declare(strict_types=1);

namespace App;

// A `(string)` of a value only PHPDoc types a string refuses: the value may be another type at runtime, and a PHP# cast
// converts a number (spec section 24).
final class Labels
{
    public function label(Row $row): string
    {
        return (string) $row->label;
    }
}
