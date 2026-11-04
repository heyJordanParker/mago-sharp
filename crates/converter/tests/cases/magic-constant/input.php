<?php

declare(strict_types=1);

namespace App;

// `__DIR__`, `__FILE__` and `__LINE__` are what `Position.current()` gives (spec section 27).
final class Paths
{
    public function stubs(): string
    {
        $file = __FILE__;

        return __DIR__ . '/stubs:' . __LINE__ . $file;
    }
}
