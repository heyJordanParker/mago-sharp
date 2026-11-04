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

    // Method values: `name(...)` and `[$this, 'name']` where a function is declared.
    public function halved(int $amount): int
    {
        $less = $this->less(...);

        return $this->apply([$this, 'less'], $less($amount));
    }

    public function less(int $amount): int
    {
        return $amount - 2;
    }

    /** @param callable(int): int $rule */
    private function apply(callable $rule, int $amount): int
    {
        return $rule($amount);
    }

    // Spreads into a `...` parameter and into a literal, and `new` of a class value.
    public function count(int ...$amounts): int
    {
        $total = 0;
        foreach ($amounts as $amount) {
            $total += $amount;
        }

        return $total;
    }

    /** @param list<int> $more */
    public function counted(array $more): int
    {
        $all = [...$more, 4];

        return $this->count(...[1, 2], ...$all);
    }

    /** @param class-string<Pair> $type */
    public function made(string $type): string
    {
        return (new $type())->both();
    }
}
