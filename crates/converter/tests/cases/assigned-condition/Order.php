<?php

declare(strict_types=1);

namespace App;

final class Order
{
    public function __construct(public int $total)
    {
    }
}
