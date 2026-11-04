<?php

declare(strict_types=1);

namespace App;

// `match` keeps its form: an arm's constants become patterns joined by `or`, and a `match` that stands as a statement
// takes block arms (spec section 21).
final class Router
{
    private int $hits = 0;

    public function size(int $code): string
    {
        return match ($code) {
            200, 201 => 'ok',
            404 => 'missing',
            default => 'other',
        };
    }

    public function count(string $method): void
    {
        match ($method) {
            'GET' => $this->hits++,
            default => $this->hits += 2,
        };
    }
}
