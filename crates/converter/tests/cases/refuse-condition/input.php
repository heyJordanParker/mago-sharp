<?php

declare(strict_types=1);

namespace App;

// PHP reads an empty array as false, and no PHP# pattern matches an empty `Map` among a union's other types or a
// type parameter's value, which has no `==` without a bound, while a string a second read may not give again, as a
// call's, is read once by a `match` (spec section 21).
final class Untested
{
    private string $name = '';

    /** @var list<int> */
    private array $counts = [];

    /** @param array<string, int>|int $value */
    public function keyed(array|int $value): bool
    {
        return !$value;
    }

    /**
     * @template T
     *
     * @param T $value
     */
    public function held(mixed $value): bool
    {
        return !$value;
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
