<?php

declare(strict_types=1);

namespace App;

// PHP reads an object as true, and `null`, `false`, `0`, `0.0`, `""`, `"0"` and an empty array as false, so a condition
// of an object that is never null is `true`, and one of a value of no known type or of a union of scalars is one read
// through a `match` on the values PHP reads as false (spec section 21).
final class Truthy
{
    public function never(Truthy $other): bool
    {
        return !$other;
    }

    public function made(): bool
    {
        return $this->other() ? true : false;
    }

    public function unknown(mixed $value): bool
    {
        return $value ? true : false;
    }

    public function either(int|float $value): bool
    {
        return !$value;
    }

    public function kept(Truthy $other, Truthy $fallback): Truthy
    {
        return $other ?: $fallback;
    }

    private function other(): Truthy
    {
        return $this;
    }
}
