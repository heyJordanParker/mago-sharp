<?php

declare(strict_types=1);

namespace App;

// Cases between `:` and `endswitch` are cases in braces, and a case that joins `default` goes, since `default` covers
// it (spec section 21).
final class Sizes
{
    public function size(string $kind): int
    {
        switch ($kind):
            case 'large':
                return 3;
            case 'small':
            case 'tiny':
            default:
                return 1;
        endswitch;
    }
}
