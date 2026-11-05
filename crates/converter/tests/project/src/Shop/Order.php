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
}
