<?php

declare(strict_types=1);

namespace App;

// `replace` puts the new text before each character for an empty text to replace, where `str_replace` leaves the text
// as it is, so `str_replace` of a text that may be empty has no PHP# form.
final class Slug
{
    public function swap(string $from, string $title): string
    {
        return str_replace($from, '-', $title);
    }
}
