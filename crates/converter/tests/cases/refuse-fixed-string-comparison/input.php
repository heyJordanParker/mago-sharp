<?php

declare(strict_types=1);

namespace App;

// A comparison of a string with a string fixed at compile time selects one of a closed set of values, which an
// application declares as a backed enum: `==` with a literal, with a class's name, which PHP compiles in for `::class`,
// and a `match` arm (spec section 24). The file stays plain PHP.
final class Reply
{
    public function agreed(string $answer): bool
    {
        return $answer === 'yes';
    }

    public function special(?string $name): bool
    {
        return $name === Reply::class;
    }

    public function routed(string $method): int
    {
        return match ($method) {
            'GET' => 1,
            default => 0,
        };
    }
}
