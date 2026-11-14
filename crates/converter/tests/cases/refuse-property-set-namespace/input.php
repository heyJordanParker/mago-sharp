<?php

declare(strict_types=1);

namespace App\Billing;

// Code of another namespace sets `status`, which a PHP# property's `set` never reaches (spec section 6).
final class Transaction
{
    public string $status = 'open';

    public function __construct(public int $total)
    {
    }
}
