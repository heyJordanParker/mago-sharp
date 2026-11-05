<?php

declare(strict_types=1);

namespace App;

class Magic
{
    public ?string $value = null;

    public ?string $name;

    public function __isset(string $name): bool
    {
        return true;
    }
}
