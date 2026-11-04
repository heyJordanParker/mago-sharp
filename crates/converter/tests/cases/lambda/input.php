<?php

declare(strict_types=1);

namespace App;

// `fn` and `function` are lambdas, which capture each variable they use with no `use` list. A capture by reference
// shares the variable as PHP# does, and one by value keeps PHP's meaning when nothing writes the variable again (spec
// section 14.2). A closure type is a function type (spec section 14.1).
final class Totals
{
    public function total(int $base, int $factor): int
    {
        $offset = 1;
        $scale = fn(int $amount): int => $amount * $factor + $offset;
        $sum = 0;
        $add = function (int $amount) use (&$sum, $scale): void {
            $sum += $scale($amount);
        };
        $add($base);
        $add($base + 1);

        return $sum;
    }

    /** @return \Closure(int): int */
    public function adder(int $step): \Closure
    {
        return static fn(int $value): int => $value + $step;
    }
}
