<?php

declare(strict_types=1);

namespace App;

// A condition that is no `bool` reads PHP's truthiness, which PHP# writes as the comparison its type gives (spec
// section 21).
final class Truthy
{
    /** @var list<string> */
    private array $lines = [];

    /** @var array<string, int> */
    private array $stock = [];

    public function missing(string $sku): bool
    {
        return !isset($this->stock[$sku]) || !empty($this->lines);
    }

    public function named(int $count): int
    {
        if ($count) {
            return $count;
        }

        return 1;
    }

    public function shown(?float $ratio): string
    {
        return $ratio ? 'named' : 'blank';
    }

    public function both(?Truthy $other, bool $flag): bool
    {
        return $flag && $other;
    }

    public function none(int $count): bool
    {
        return !$count;
    }

    public function some(): bool
    {
        return !empty($this->lines) and $this->lines;
    }

    public function given(?int $count, float $ratio): bool
    {
        return $count || !$ratio;
    }

    public function filled(?string $name): bool
    {
        return isset($name) || empty($name);
    }
}
