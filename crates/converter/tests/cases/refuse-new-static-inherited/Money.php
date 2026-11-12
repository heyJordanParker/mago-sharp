<?php

declare(strict_types=1);

namespace App;

class Money
{
    public function __construct(public readonly int $cents)
    {
    }
}
