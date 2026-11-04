<?php

declare(strict_types=1);

namespace Proof\Shop;

// Lambdas: a capture by reference shares the variable, and one by value reads a variable nothing writes again.
final class Tally
{
    public function sum(int $first, int $second, int $factor): int
    {
        $offset = 1;
        $scale = fn(int $amount): int => $amount * $factor + $offset;
        $sum = 0;
        $add = function (int $amount) use (&$sum, $scale): void {
            $sum += $scale($amount);
        };
        $add($first);
        $add($second);

        return $sum;
    }

    /** @return \Closure(int): int */
    public function adder(int $step): \Closure
    {
        return static fn(int $value): int => $value + $step;
    }
}
