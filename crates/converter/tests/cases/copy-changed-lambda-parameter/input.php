<?php

declare(strict_types=1);

namespace App;

// A lambda that changes its own parameter copies it into a `let` local first, as a method does, and an arrow function
// becomes a lambda with a body in braces for it (spec sections 2 and 14.2).
final class Steps
{
    public function next(int $start): int
    {
        $step = fn(int $count): int => $count += 1;
        $grow = function (int $count): int {
            $count *= 2;

            return $count;
        };

        return $grow($step($start));
    }
}
