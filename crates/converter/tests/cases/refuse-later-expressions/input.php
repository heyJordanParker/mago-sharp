<?php

declare(strict_types=1);

namespace App;

// Destructuring by position, `print`, `new` of a string and generators have no form or wait, while the lambda, the
// `match`, the `clone`, the template, the magic constant and the spreads beside them convert.
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
