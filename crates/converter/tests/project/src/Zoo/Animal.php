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

    // A call through `self` of a static method that reads no late static binding, called through a subclass.
    public static function counted(int $animals): string
    {
        return $animals . ' ' . self::plural($animals);
    }

    private static function plural(int $count): string
    {
        return $count === 1 ? 'animal' : 'animals';
    }

    public function describe(): string
    {
        return $this->name . ' ' . $this->kind() . ' says ' . $this->sound . ' on ' . $this->legs . ' legs';
    }

    // A method declared `array` that a converted subclass overrides with a `Map`, whose result this class's own code
    // reads as the PHP array while it is plain PHP.
    /** @return array<string, int> */
    public function traits(): array
    {
        return ['legs' => $this->legs];
    }

    public function summary(): string
    {
        $text = '';
        foreach ($this->traits() as $trait => $count) {
            $text .= $trait . '=' . $count . ';';
        }

        return $text;
    }
}
