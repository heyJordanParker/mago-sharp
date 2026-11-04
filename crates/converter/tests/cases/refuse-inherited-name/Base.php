<?php

declare(strict_types=1);

namespace App;

class Base
{
    protected string $title = 'base';

    public function label(): string
    {
        return 'base';
    }
}
