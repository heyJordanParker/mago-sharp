<?php

declare(strict_types=1);

namespace Proof\Zoo;

// A subclass project.sh converts before its parent.
final class Cat extends Animal
{
    public string $sound = 'meow';

    public function __construct(string $name)
    {
        parent::__construct($name, 4);
    }

    public function kind(): string
    {
        return 'cat';
    }

    public function describe(): string
    {
        return 'quietly, ' . parent::describe();
    }

    /** @return array<string, int> */
    public function traits(): array
    {
        return ['legs' => 4, 'lives' => 9];
    }
}
