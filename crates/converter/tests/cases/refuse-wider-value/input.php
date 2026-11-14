<?php

declare(strict_types=1);

namespace App;

// PHP returns, and passes, a value its declared type may not hold where `as` tests another thing than PHP checks: a
// class value, a type the value never holds and a type that allows null. It reads a missing key as `null`. PHP# refuses
// each (spec sections 7, 12 and 21).
final class Drawing
{
    /** @var array<string, int> */
    private array $sizes = ['small' => 1];

    /** @return class-string<Shape> */
    public function kind(): string
    {
        return $this->anyKind();
    }

    public function draw(int $size): string
    {
        return $this->square($size);
    }

    public function describe(?Shape $shape): string
    {
        return Square::describe($shape);
    }

    public function size(string $name): int
    {
        return $this->sizes[$name];
    }

    /** @return class-string */
    private function anyKind(): string
    {
        return Square::class;
    }

    private function square(string $square): string
    {
        return 'square';
    }
}
