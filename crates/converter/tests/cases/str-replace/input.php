<?php

declare(strict_types=1);

namespace App;

// `str_replace` of a literal text other than `""` is `replace`, which runs it for such a text, while `replace` puts the
// new text before each character for an empty one, where PHP leaves the text as it is.
final class Slug
{
    public function make(string $title): string
    {
        return str_replace(' ', '-', $title);
    }
}
