<?php

declare(strict_types=1);

namespace App\Orders;

use App\Events\ReceiveEvent;

// A plain PHP class takes `dispatch` from a plain PHP trait, and the trait's `extern` line declares the effect of that
// one body, so a call through the class has it, which a PHP# enum never makes (spec section 29).
enum Status: string
{
    case Open = 'open';

    public function announce(): void
    {
        ReceiveEvent::dispatch($this->value);
    }
}
