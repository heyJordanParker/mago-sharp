<?php

declare(strict_types=1);

namespace App;

// PHP# names a constant in upper snake case, and a field, a property, a method, a parameter and a local in camel case
// (spec section 24). Each is a name PHP reaches the code by, so the PHP takes the new name first.
final class Names
{
    public const Kind = 'kind';

    public const MAX_SIZE = 3;

    public string $Label = '';

    private int $item_count = 0;

    public function Total(int $unit_price): int
    {
        $line_total = $unit_price * $this->item_count;

        return $line_total;
    }
}
