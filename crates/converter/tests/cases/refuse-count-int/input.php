<?php

declare(strict_types=1);

namespace App;

// PHP's `count` of a value no standard library member counts keeps the file plain PHP, and the refusal names the type
// each member takes as PHP# writes it (decision 083).
final class Tally
{
    public function size(int $value): int
    {
        return count($value);
    }
}
