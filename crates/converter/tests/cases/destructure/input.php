<?php

declare(strict_types=1);

namespace App;

// Destructuring a `Map` by key reads each key into its own local, a `TValue?`, as PHP gives `null` for a missing key
// (spec section 12).
final class Rows
{
    /** @param array<string, int> $row */
    public function total(array $row): int
    {
        ['price' => $price, 'count' => $count] = $row;

        return ($price ?? 0) * ($count ?? 1);
    }
}
