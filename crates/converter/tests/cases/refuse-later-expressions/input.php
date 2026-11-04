<?php

declare(strict_types=1);

namespace App;

// Functions as values, destructuring, interpolation, magic constants, `print`, `new` of a class value, spreads and
// generators wait for later families, while the `match` and the `clone` beside them convert.
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
        $printed = print $name;
        $made = new $name();
        $joined = $this->join(...[$name]);
        $all = [...[$name]];

        return [$double, $size, $copy, $first, $second, $greeting, $file, $printed, $made, $joined, $all];
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
