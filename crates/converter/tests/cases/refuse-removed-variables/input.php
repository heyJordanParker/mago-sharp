<?php

declare(strict_types=1);

namespace App;

// `$$name`, `global` and the superglobals are removed (spec sections 2 and 29).
final class Globals
{
    public function named(string $name): mixed
    {
        return $$name;
    }

    public function shared(): void
    {
        global $config;
    }

    public function host(): mixed
    {
        return $_SERVER;
    }
}
