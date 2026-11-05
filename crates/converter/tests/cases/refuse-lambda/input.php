<?php

declare(strict_types=1);

namespace App;

// A capture by value keeps PHP's meaning only when nothing writes the variable again, in the lambda or after it, and
// PHP# calls a function value only (spec sections 14.1 and 14.2).
final class Counters
{
    public function run(int $count, string $name): int
    {
        $limit = 3;
        $check = fn(int $value): bool => $value < $limit;
        $limit = 4;
        $bump = function () use ($count): int {
            $count++;

            return $count;
        };
        $name();

        return $limit + ($check(1) ? 1 : 0) + $bump();
    }

    // A local read after its block is declared first in the method, so each of its assignments writes it after its
    // declaration.
    public function hoisted(bool $flag): int
    {
        if ($flag) {
            $base = 2;
        } else {
            $base = 5;
        }
        $scale = fn(int $value): int => $value * $base;

        return $scale(3);
    }
}
