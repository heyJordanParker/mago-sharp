<?php

declare(strict_types=1);

namespace App;

use function Acme\Slug\slugify;

// A plain PHP function no `extern` line declares may have any effect, as the `uses any` line `mago convert` writes
// for it says, so an enum that calls it stays plain PHP (spec section 29).
enum Topic: string
{
    case Billing = 'Billing';

    public function slug(): string
    {
        return slugify($this->value);
    }
}
