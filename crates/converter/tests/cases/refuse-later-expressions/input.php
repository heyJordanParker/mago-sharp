<?php

declare(strict_types=1);

namespace App;

// Functions as values, `match`, `clone`, destructuring, interpolation, magic constants, `isset` and `empty`, `new` of a
// class value, spreads and generators wait for later slices.
final class Later
{
    public function run(string $name, Later $other, int $count): mixed
    {
        $double = fn(int $value): int => $value * 2;
        $size = match ($count) {
            1 => 'one',
            default => 'many',
        };
        $copy = clone $other;
        [$first, $second] = [1, 2];
        $greeting = "Hello $name";
        $file = __FILE__;
        $known = isset($this->missing);
        $made = new $name();
        $joined = $this->join(...[$name]);
        $all = [...[$name]];

        return [$double, $size, $copy, $first, $second, $greeting, $file, $known, $made, $joined, $all];
    }

    public function join(string ...$names): string
    {
        return '';
    }

    public function numbers(): iterable
    {
        yield 1;
    }
}
