<?php

declare(strict_types=1);

namespace App;

// An enum's header lists its backing type and its interfaces after `:`, its cases keep their form, and no method of
// it is `virtual` (spec section 20).
enum Status: string implements Labeled
{
    case Open = 'open';
    case Closed = 'closed';

    public function label(): string
    {
        return $this === self::Open ? 'Open' : 'Closed';
    }
}
