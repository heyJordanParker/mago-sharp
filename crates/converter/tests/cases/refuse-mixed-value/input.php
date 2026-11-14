<?php

declare(strict_types=1);

namespace App;

// PHP lets a value of no known type reach an operation of a known type, and PHP# lets it only after `is` narrows it
// (spec section 24), or `as` and `match` where PHP checks the type, which a class PHP# can't name has none of.
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
        return $value === 1;
    }

    public function key($value): int
    {
        return $this->scores[$value];
    }

    public function node($value): void
    {
        (new Nodes())->append($value);
    }
}
