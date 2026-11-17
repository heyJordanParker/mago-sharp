<?php

declare(strict_types=1);

namespace App;

// Cases between `:` and `endswitch` are cases in braces, and a case that joins `default` goes, since `default` covers
// it (spec section 21).
final class Sizes
{
    public function size(int $weight): int
    {
        switch ($weight):
            case 30:
                return 3;
            case 20:
            case 10:
            default:
                return 1;
        endswitch;
    }
}
