<?php

declare(strict_types=1);

namespace App;

// A `(string)` of a value that may be one of several types refuses: a PHP# cast converts a number, and a template shows
// one type, so neither shows `int|string` PHP's way (spec sections 18 and 24).
final class Labels
{
    public function code(Row $row): string
    {
        return (string) $row->code;
    }
}
