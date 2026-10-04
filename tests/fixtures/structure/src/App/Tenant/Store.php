<?php

namespace App\Tenant;

use App\Shared\Money;

final class Store
{
    public function price(): Money
    {
        return new Money();
    }
}
