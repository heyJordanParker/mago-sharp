<?php

declare(strict_types=1);

namespace App;

/** @deprecated Use Store instead. */
class Base
{
    public function path($path = ''): string
    {
        return '/' . $path;
    }
}
