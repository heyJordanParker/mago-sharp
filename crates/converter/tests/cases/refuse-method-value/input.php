<?php

declare(strict_types=1);

namespace App;

// PHP# reaches a built-in through the standard library member that runs it, and names a method value on the class
// or object that has the method (spec section 14.3).
final class Values
{
    /** @param callable(string): int $rule */
    public function run(callable $rule): int
    {
        return $rule('a');
    }

    public function both(): int
    {
        $length = strlen(...);

        return $this->run([$this, 'missing']) + $length('ab');
    }
}
