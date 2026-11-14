<?php

declare(strict_types=1);

namespace App;

/**
 * A value the analysis types wider than a type parameter whose bound holds null has no PHP# form: `as T` gives null
 * for a value that is no `T`, and for a null `T` too, which PHP returns or passes and `?? throw` would throw for
 * (spec section 21).
 *
 * @template T
 */
final class Transaction
{
    public function __construct(private mixed $found)
    {
    }

    /** @return T */
    public function run(): mixed
    {
        return $this->found;
    }

    public function keepFound(): void
    {
        $this->keep($this->found);
    }

    /** @param T $item */
    private function keep(mixed $item): void
    {
    }
}
