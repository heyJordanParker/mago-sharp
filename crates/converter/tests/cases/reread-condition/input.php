<?php

declare(strict_types=1);

namespace App;

// PHP's truthiness of a string is a test of several parts, and so is `?:`, which tests a value and gives it. Each reads
// the value again, which gives what PHP's one read gives for a variable, and for a property its class declares with no
// `get` hook and no `__get` (spec section 21).
final class Customer
{
    private ?string $nickname = null;

    public function __construct(private string $name)
    {
    }

    public function greeting(?string $title): string
    {
        if ($this->nickname) {
            return 'Hi ' . $this->nickname;
        }

        return ($title ?: 'Dear') . ' ' . ($this->name ?: 'customer');
    }
}
