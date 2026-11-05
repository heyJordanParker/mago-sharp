<?php

declare(strict_types=1);

namespace App;

// PHP# has no references: a parameter, a return and a value are never taken by reference (spec section 14).
final class Counter
{
    private int $count = 0;

    public function bump(int &$count): void
    {
        $count++;
    }

    public function &current(): int
    {
        return $this->count;
    }

    public function alias(): int
    {
        $alias = &$this->count;

        return $alias;
    }
}
