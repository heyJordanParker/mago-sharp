<?php

declare(strict_types=1);

namespace Proof\Shop;

final class Note
{
    public function describe($value): string
    {
        if (is_string($value)) {
            return 'text ' . $value;
        }

        return 'other';
    }
}
