<?php

declare(strict_types=1);

namespace App;

// PHP lets a value of no known type reach an operation of a known type, and PHP# lets it only after `is` narrows it
// (spec section 24).
final class Loose
{
    /** @var array<string, int> */
    private array $scores = [];

    public function access($value): string
    {
        return $value->name;
    }

    public function call($value): string
    {
        return $value->run();
    }

    public function cast($value): string
    {
        return (string) $value;
    }

    public function compare($value): bool
    {
        return $value === 'yes';
    }

    public function key($value): int
    {
        return $this->scores[$value];
    }

    public function argument($value): string
    {
        return $this->greet($value);
    }

    public function greet(string $name): string
    {
        return 'hi ' . $name;
    }
}
