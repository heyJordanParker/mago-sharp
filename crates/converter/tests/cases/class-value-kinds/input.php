<?php

declare(strict_types=1);

namespace App;

// `class-string<T>` is a `Class<T>` (spec section 25).
final class Kinds
{
    /**
     * @template T of Kinds
     * @param class-string<T> $kind
     * @return class-string<T>
     */
    public function same(string $kind): string
    {
        return $kind;
    }
}
