<?php

declare(strict_types=1);

namespace App;

// `===` of two class values is `==`, which compares the names as PHP's `===` does, where neither may be the class of an
// object of a PHP# generic class, or one is a class that takes no type arguments, and so is `===` of an `array-key`,
// which is `int|string`, with a string. A class value is always truthy, so its test is its null check.
final class Router
{
    /**
     * @param class-string<Router> $handler
     * @param class-string<Router>|null $fallback
     */
    public function same(string $handler, ?string $fallback): bool
    {
        return $handler === $fallback;
    }

    /** @param class-string|null $fallback */
    public function routed(?string $fallback): bool
    {
        if ($fallback) {
            return $fallback !== Router::class;
        }

        return false;
    }

    /** @param array-key $key */
    public function isId(int|string $key): bool
    {
        return $key === 'id';
    }
}
