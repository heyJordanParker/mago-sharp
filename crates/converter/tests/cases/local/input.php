<?php

declare(strict_types=1);

namespace App;

// A local's first assignment declares it: `const` when nothing writes it again, `let` when something does.
final class Ledger
{
    public function total(int $base): int
    {
        $let = 2;
        $sum = $base + $let;
        $sum += 1;
        $count = 0;
        $count++;
        if ($sum > 2) {
            $step = 1;
            $count = $count + $step;
        }
        $result = $sum + $count;

        return $result;
    }
}
