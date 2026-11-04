<?php

declare(strict_types=1);

namespace App;

// PHP# declares no traits (spec section 22).
trait Greets
{
    public function greet(): string
    {
        return 'hi';
    }
}
