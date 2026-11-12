<?php

declare(strict_types=1);

namespace Proof\Shop;

use Proof\Spec;

final class Order
{
    private int $quantity = 0;

    public function __construct(private Spec $spec, public string $reference)
    {
    }

    public function add(int $count): void
    {
        $this->quantity += $count;
    }

    public function summary(): string
    {
        return $this->reference . ': ' . $this->spec->label() . ' x' . $this->quantity;
    }

    public function isFor(string $reference): bool
    {
        return $this->reference === $reference;
    }

    public function isOf(Spec $spec): bool
    {
        return $this->spec === $spec;
    }

    public function loud(): string
    {
        return shout($this->reference);
    }

    // `!==` between a union of scalars and `false`, which PHP#'s `!=` gives with the same types and values.
    public function found(string|bool $position): string
    {
        return $position !== false ? 'found' : 'missing';
    }

    // An intersection type, and a `class-string`, which the engine checks where each enters PHP#.
    public function labelled(Spec&\Countable $spec): string
    {
        return $spec->label();
    }

    /**
     * @param class-string $type
     * @return class-string
     */
    public function same(string $type): string
    {
        return $type;
    }
}
