<?php

declare(strict_types=1);

namespace App;

// PHP's count of loops to leave, as `break 2`, is the name of the loop it leaves in PHP#, which the loop takes, and a
// count of 1 is the innermost loop, which a plain `break` leaves (spec section 17).
final class Grid
{
    /** @param list<list<int>> $rows */
    public function first(array $rows): int
    {
        $found = 0;
        foreach ($rows as $row) {
            foreach ($row as $cell) {
                if ($cell > 0) {
                    $found = $cell;
                    break 2;
                }
                if ($cell < 0) {
                    continue 2;
                }
                if ($cell === 0) {
                    break 1;
                }
            }
        }

        return $found;
    }
}
