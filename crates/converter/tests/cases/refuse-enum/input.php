<?php

declare(strict_types=1);

namespace App;

// A later slice converts an enum (spec section 20).
enum Status: string
{
    case Paid = 'paid';
}
