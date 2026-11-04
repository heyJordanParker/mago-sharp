<?php

declare(strict_types=1);

namespace App;

// PHP returns, and passes, a value its declared type may not hold, and reads a missing key as `null`. PHP# refuses each
// (spec sections 7 and 12).
final class Drawing
{
    /** @var array<string, int> */
    private array $sizes = ['small' => 1];

    /** @return class-string<Shape> */
    public function kind(): string
    {
        return $this->anyKind();
    }

    public function draw(Shape $shape): string
    {
        return $this->square($shape);
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

    private function square(Square $square): string
    {
        return 'square';
    }
}
