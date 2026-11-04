<?php

declare(strict_types=1);

namespace App;

// A condition that is no `bool` reads PHP's truthiness, which a later slice writes as the comparison its type gives
// (spec section 21).
final class Truthy
{
    public function named(string $name): string
    {
        if ($name) {
            return $name;
        }

        return 'blank';
    }

    public function shown(string $name): string
    {
        return $name ? 'named' : 'blank';
    }

    public function both(?Truthy $other, bool $flag): bool
    {
        return $flag && $other;
    }

    public function none(int $count): bool
    {
        return !$count;
    }
}
