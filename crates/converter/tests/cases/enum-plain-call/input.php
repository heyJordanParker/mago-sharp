<?php

declare(strict_types=1);

namespace App;

use function Acme\Slug\slugify;

// A plain PHP function no `extern` declares has no effect the checker names, so an enum that calls it converts, as a
// struct does (spec section 29).
enum Topic: string
{
    case Billing = 'Billing';

    public function slug(): string
    {
        return slugify($this->value);
    }
}
