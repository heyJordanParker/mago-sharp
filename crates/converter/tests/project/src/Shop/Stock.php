<?php

declare(strict_types=1);

namespace Proof\Shop;

use Acme\Format\Record;

// Values the engine checks where they enter PHP#: an `(int)` of a property only PHPDoc types `int` stays, since the
// property may hold `"5"`, and an `iterable` the body loops over by value alone takes any keys and class.
final class Stock
{
    public function quantity(Record $record): int
    {
        return (int) $record->quantity;
    }

    /** @param iterable<string> $names */
    public function joined(iterable $names): string
    {
        $joined = '';
        foreach ($names as $name) {
            $joined .= $name;
        }

        return $joined;
    }
}
