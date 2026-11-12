<?php

declare(strict_types=1);

namespace App;

// `match (true)` runs the arm of the first condition that is `true`, which PHP compares by `===`, so for `bool`
// conditions it is a chain of ternaries, or `if … else if … else` as a statement (spec section 21).
final class Shipping
{
    public string $note = '';

    public function rate(int $weight, bool $express, bool $abroad): string
    {
        return match (true) {
            $weight > 30 => 'freight',
            $express, $abroad => 'priority',
            default => 'standard',
        };
    }

    // An operator around the chain binds tighter than `?:`, so the chain takes parentheses, and a local's value takes
    // none.
    public function fee(bool $express, int $extra): int
    {
        $base = match (true) {
            $express => 5,
            default => 0,
        };

        return match (true) {
            $express => 5,
            default => 0,
        } + $extra + $base;
    }

    public function mark(bool $fragile, bool $heavy): void
    {
        match (true) {
            $fragile => $this->note = 'fragile',
            $heavy => $this->note = 'heavy',
            default => $this->note = '',
        };
    }
}
