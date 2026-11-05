<?php

declare(strict_types=1);

namespace App;

// `__construct` is the class's name, and a promoted parameter declares a field or a property (spec section 9).
final class Order
{
    public function __construct(
        private Spec $spec,
        public string $reference,
        public readonly int $quantity = 1,
        string $note = '',
    ) {
    }

    public function current(): Spec
    {
        return $this->spec;
    }
}
