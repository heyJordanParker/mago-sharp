<?php

declare(strict_types=1);

namespace App;

// A returned value the analysis types wider than the method's return is checked as that type, which PHP checks where
// the method returns and throws a `TypeError` for any other value (spec section 21).
final class Lookup
{
    public function name(mixed $value): string
    {
        return $value;
    }

    public function owner(?Lookup $lookup): Lookup
    {
        return $lookup;
    }

    public function enabled(bool|int $flag): bool
    {
        return $flag;
    }
}
