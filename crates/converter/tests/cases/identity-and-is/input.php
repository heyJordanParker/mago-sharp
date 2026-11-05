<?php

declare(strict_types=1);

namespace App;

// `===` is `==` where both sides have one scalar type or a side is null, and stays where a side may hold an object
// (spec section 19). `instanceof` and the type tests are `is` and `is not` (spec section 21).
final class Check
{
    public function same(string $a, string $b): bool
    {
        return $a === $b;
    }

    public function differ(int $a, int $b): bool
    {
        return $a !== $b;
    }

    public function missing(?Spec $spec): bool
    {
        return $spec === null;
    }

    public function exactly(Spec $a, Spec $b): bool
    {
        return $a === $b;
    }

    public function special(Spec $spec): bool
    {
        return $spec instanceof Special;
    }

    public function plain(Spec $spec): bool
    {
        return !$spec instanceof Special;
    }

    public function text(mixed $value): bool
    {
        return is_string($value);
    }

    public function notText(mixed $value): bool
    {
        return !is_string($value);
    }

    public function nothing(mixed $value): bool
    {
        return is_null($value);
    }
}
