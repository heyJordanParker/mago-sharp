<?php

declare(strict_types=1);

namespace App;

// A library compares strings fixed at compile time and reads a `Map` at one, which an application refuses, so it keeps
// each form: a string's truthiness, `==` with a literal and with a class's name, a `match` arm, a fixed `Map` key, and
// a constant of a class held at run time, which reflection finds by its name (spec sections 21, 24 and 25).
final class Matcher
{
    public function named(string $name): string
    {
        return $name ? $name : 'guest';
    }

    public function agreed(string $answer): bool
    {
        return $answer === 'yes';
    }

    public function special(?string $name): bool
    {
        return $name === Matcher::class;
    }

    public function routed(string $method): int
    {
        return match ($method) {
            'GET' => 1,
            default => 0,
        };
    }

    /** @param array<string, int> $sizes */
    public function small(array $sizes): int
    {
        return $sizes['small'];
    }

    /** @param class-string<Order> $kind */
    public function limit(string $kind): int
    {
        return $kind::MAX * 2;
    }
}
