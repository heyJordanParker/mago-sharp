<?php

declare(strict_types=1);

namespace App;

// A `set` hook that changes `$value` copies it into a `let` local first, as a method does with its parameter, and a
// short `set` becomes a body in braces for it (spec sections 2 and 6).
final class Counter
{
    public int $count = 0 {
        set => ++$value;
    }

    public int $twice = 0 {
        set {
            $value = $value * 2;
            $this->twice = $value;
        }
    }

    public function store(int $amount): int
    {
        $this->count = $amount;
        $this->twice = $amount;

        return $this->count + $this->twice;
    }
}
