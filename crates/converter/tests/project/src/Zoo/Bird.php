<?php

declare(strict_types=1);

namespace Proof\Zoo;

// A subclass project.sh converts after its parent.
final class Bird extends Animal
{
    public string $sound = 'tweet';

    public function __construct(string $name)
    {
        parent::__construct($name, 2);
    }

    public function kind(): string
    {
        return 'bird';
    }
}
