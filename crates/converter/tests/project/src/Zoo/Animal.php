<?php

declare(strict_types=1);

namespace Proof\Zoo;

// The base of a hierarchy project.sh converts in either order: a subclass before it, then it, then a subclass after
// it. Its properties are `virtual`, so a converted subclass replaces them.
abstract class Animal
{
    public string $sound = '...';

    public string $name {
        get => $this->name . '!';
        set => 'the ' . $value;
    }

    public function __construct(string $name, protected int $legs = 4)
    {
        $this->name = $name;
    }

    abstract public function kind(): string;

    public function describe(): string
    {
        return $this->name . ' ' . $this->kind() . ' says ' . $this->sound . ' on ' . $this->legs . ' legs';
    }
}
