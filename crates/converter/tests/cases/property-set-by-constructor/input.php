<?php

declare(strict_types=1);

namespace App;

// A typed property with no initial value converts when the constructor sets it on every path.
final class Ticket
{
    private string $code;

    public function __construct(string $prefix)
    {
        $this->code = $prefix . '-1';
    }

    public function label(): string
    {
        return $this->code;
    }
}
