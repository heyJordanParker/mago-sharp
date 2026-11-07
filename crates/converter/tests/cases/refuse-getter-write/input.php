<?php

declare(strict_types=1);

namespace App;

// A PHP `get` hook may keep a value it computes in a field, and a PHP# `get` body never writes `this` (spec section
// 6.1).
final class Settings
{
    public string $label {
        get => $this->cached ??= $this->name . '!';
    }

    private ?string $cached = null;

    public function __construct(private string $name)
    {
    }
}
