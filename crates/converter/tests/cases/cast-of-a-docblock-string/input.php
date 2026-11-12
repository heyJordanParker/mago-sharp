<?php

declare(strict_types=1);

namespace App;

// A `(string)` of a value only PHPDoc types a string is a template: the value may be another type at runtime, and a
// template shows a `string` as PHP's interpolation does, which converts any value to text as `(string)` does, and a
// `null` as `""` once `?? ""` gives it (spec sections 18 and 24).
final class Labels
{
    public function label(Row $row): string
    {
        return (string) $row->label . ' ' . (string) $row->note;
    }
}
