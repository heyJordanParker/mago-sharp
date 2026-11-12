<?php

declare(strict_types=1);

namespace Proof;

class Spec
{
    public function __construct(public readonly string $name)
    {
    }

    public function label(): string
    {
        return 'spec ' . $this->name;
    }

    // `instanceof self` tests this class, which a subclass's object is too.
    public function sameKind(object $other): bool
    {
        return $other instanceof self;
    }
}
