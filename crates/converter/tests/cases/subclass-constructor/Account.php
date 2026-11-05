<?php

declare(strict_types=1);

namespace App;

class Account
{
    public function __construct(protected string $owner)
    {
    }
}
