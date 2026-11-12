<?php

declare(strict_types=1);

namespace Proof\Shop;

use Acme\Format\Stamp;

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

    // A read of a key the row may lack converts behind `??`, which PHP reads without a warning.
    /** @param array<string, int> $row */
    public function priced(array $row): int
    {
        return ($row['price'] ?? 0) * ($row['count'] ?? 1);
    }

    // A local one pass of a loop assigns keeps its value into the next pass, as PHP's variable lives for the method.
    // Each local and `catch` variable takes its camel case name.
    /** @param list<int> $values */
    public function carried(array $values): int
    {
        $running_sum = 0;
        foreach ($values as $value) {
            if ($value > 0) {
                $last_positive = $value;
            }
            $running_sum += $last_positive ?? 0;
        }

        return $running_sum;
    }

    public function guarded(int $count): string
    {
        try {
            $item_count = $this->checked($count);

            return "items: {$item_count}";
        } catch (\RuntimeException $not_counted) {
            return $not_counted->getMessage();
        }
    }

    // A `catch` of several classes, which catches either.
    public function bounded(int $count): string
    {
        try {
            if ($count < 0) {
                throw new \LogicException('negative');
            }

            return 'items: ' . $this->checked($count);
        } catch (\RuntimeException|\LogicException $failure) {
            return $failure->getMessage();
        }
    }

    private function checked(int $count): int
    {
        if ($count === 0) {
            throw new \RuntimeException('no items');
        }

        return $count;
    }

    // PHP's own constants, which PHP# reads through the standard library's `Int.MAX` and `Int.MIN`.
    public function bounds(): string
    {
        return PHP_INT_MAX . ' ' . \PHP_INT_MIN;
    }

    // A local renamed to camel case beside a static property of its name, which keeps the name its class declares.
    public function stepped(int $count): int
    {
        $step_size = Counter::$step_size;

        return $count * $step_size;
    }

    // An `if` whose condition first assigns a local that a comparison reads, which runs before the rest of it.
    public function graded(int $amount): string
    {
        if (($half = $this->halved($amount)) >= 2 && $half !== 3) {
            return 'large ' . $half;
        }

        return 'small';
    }

    /** @param class-string<Pair> $type */
    public function made(string $type): string
    {
        return (new $type())->both();
    }

    // `with` runs as PHP's `clone`, so the copy's `__clone` runs.
    public function stamped(Stamp $stamp): int
    {
        $copy = clone $stamp;

        return $copy->copies;
    }
}
