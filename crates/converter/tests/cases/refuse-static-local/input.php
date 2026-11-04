<?php

declare(strict_types=1);

namespace App;

// A `static` local is a private static field in PHP#, which the converter doesn't write yet (spec section 6).
final class Ticker
{
    public function next(): int
    {
        static $count = 0;

        return ++$count;
    }
}
