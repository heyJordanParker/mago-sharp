<?php

declare(strict_types=1);

namespace App;

// PHP's truthiness has no PHP# test for an object that is never null or a value of no known type, and a string a
// second read may not give again, as a call's, is read once by a `match` (spec section 21).
final class Untested
{
    private string $name = '';

    /** @var list<int> */
    private array $counts = [];

    public function always(Untested $other): bool
    {
        return !$other;
    }

    public function unknown(mixed $value): bool
    {
        return $value ? true : false;
    }

    public function twice(): bool
    {
        return $this->named() && true;
    }

    public function index(): bool
    {
        return isset($this->counts[0]);
    }

    public function named(): string
    {
        return $this->name;
    }
}
