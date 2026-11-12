<?php

declare(strict_types=1);

namespace App;

// A `List` read past its end throws, and `getOrNull(i)` gives null there, as PHP's `??` and `isset` read a missing
// index (spec section 12).
final class Queue
{
    /** @param list<string> $jobs */
    public function next(array $jobs, int $at): string
    {
        return $jobs[$at] ?? 'none';
    }

    /** @param list<string> $jobs */
    public function has(array $jobs, int $at): bool
    {
        return isset($jobs[$at]);
    }
}
