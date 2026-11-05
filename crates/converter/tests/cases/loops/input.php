<?php

declare(strict_types=1);

namespace App;

// `foreach` is `for … of`, over a `Map`'s entries or values and a `List`'s entries, and `for`, `while` and
// `do … while` keep their form. Each body is in braces, and a `Map` key is unset with `remove` (spec sections 12 and
// 17).
final class Tally
{
    /** @var list<int> */
    private array $counts = [];

    /** @var array<string, int> */
    private array $prices = [];

    public function total(): int
    {
        $total = 0;
        foreach ($this->counts as $count) {
            $total += $count;
        }
        foreach ($this->prices as $price) {
            $total += $price;
        }
        foreach ($this->prices as $name => $price) {
            if ($name === 'free') continue;
            $total += $price;
        }
        foreach ($this->counts as $index => $count) {
            $count += $index;
            $total += $count;
        }

        return $total;
    }

    public function countdown(int $from): int
    {
        $steps = 0;
        for ($i = $from; $i > 0; $i--) {
            $steps++;
        }
        while ($steps > 10) $steps -= 10;
        do {
            $steps++;
            if ($steps > 5) {
                break;
            }
        } while ($steps < $from);

        return $steps;
    }

    public function drop(string $name): void
    {
        unset($this->prices[$name]);
    }
}
