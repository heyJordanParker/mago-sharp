<?php

declare(strict_types=1);

namespace App;

final class Email
{
    public function __construct(private string $address)
    {
    }

    public function __toString(): string
    {
        return $this->address;
    }

    public function self(): self
    {
        return $this;
    }
}
