<?php

declare(strict_types=1);

namespace App;

/**
 * A value the analysis types wider than a type parameter whose bound excludes null is checked as that type
 * parameter, which `as` tests while the code runs and gives null for a value that is no `T` (spec section 21).
 *
 * @template T of object
 */
final class Holder
{
    /** @var list<T> */
    private array $kept = [];

    public function __construct(private object $found)
    {
    }

    /** @return T */
    public function item(): object
    {
        return $this->found;
    }

    public function keepFound(): void
    {
        $this->keep($this->found);
    }

    /** @param T $item */
    private function keep(object $item): void
    {
        $this->kept[] = $item;
    }

    // A `T` lands in a `T` as it is.
    /**
     * @param T $item
     *
     * @return T
     */
    public function same(object $item): object
    {
        return $item;
    }
}
