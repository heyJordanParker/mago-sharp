<?php

declare(strict_types=1);

namespace App;

// A PHP# `let` declares one local and ends at a comma, so a `for` whose initialization declares a local beside others
// runs each initialization as a statement of its own before the loop, in its order (spec section 17).
final class Pairs
{
    public function sum(int $count): int
    {
        $total = 0;
        for ($index = 0, $last = $count - 1; $index < $last; $index++, $last--) {
            $total += $index + $last;
        }

        return $total;
    }

    // The loop keeps the name a `break` of its body gives it, after the initializations.
    /** @param list<list<int>> $rows */
    public function firstNegative(array $rows): int
    {
        for ($row = 0, $found = -1; $row < count($rows); $row++) {
            foreach ($rows[$row] as $value) {
                if ($value < 0) {
                    $found = $row;
                    break 2;
                }
            }
        }

        return $found;
    }
}
