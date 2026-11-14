<?php

declare(strict_types=1);

namespace App;

// PHP# writes a built-in's value as a lambda with the parameters of the function type it lands in, and names a method
// value on the class or object that has the method (spec sections 14.2 and 14.3).
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
