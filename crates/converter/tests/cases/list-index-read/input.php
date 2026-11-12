<?php

declare(strict_types=1);

namespace App;

// A read of a `list` at an index stays a `List` read. Past the end PHP warns and gives `null`, and PHP# throws, which
// fails where PHP warned and so loses no correct behavior (spec section 12).
final class Queue
{
    /** @param list<string> $jobs */
    public function first(array $jobs): string
    {
        return $jobs[0];
    }
}
