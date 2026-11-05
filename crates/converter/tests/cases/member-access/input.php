<?php

declare(strict_types=1);

namespace App;

// `->` and `::` are `.`, and `?->` is `?.` (spec section 4).
final class Route
{
    public ?Route $next = null;

    public string $name = 'home';

    public function nextName(): ?string
    {
        return $this->next?->name;
    }

    public function nextLabel(): ?string
    {
        return $this->next?->label();
    }

    public function label(): string
    {
        return $this->name;
    }

    public function counted(): int
    {
        return Counts::zero() + Counts::LIMIT + Counts::$total;
    }
}
