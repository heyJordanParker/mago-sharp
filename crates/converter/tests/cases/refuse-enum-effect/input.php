<?php

declare(strict_types=1);

namespace App\Orders;

use App\Support\Translator;

// A plain PHP call whose `extern` declares an effect, which a PHP# enum never makes, since an enum has no effects
// (spec section 29). A call no `extern` line covers may have any effect, so the enum never makes it either, and a call
// of the enum's own method converts with it.
enum Status: string
{
    case Open = 'open';

    public function label(): string
    {
        return Translator::get($this->value);
    }

    public function shout(): string
    {
        return Translator::upper($this->value);
    }

    public function heading(): string
    {
        return $this->shout() . '!';
    }
}
