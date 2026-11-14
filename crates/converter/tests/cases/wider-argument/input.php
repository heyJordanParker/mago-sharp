<?php

declare(strict_types=1);

namespace App;

// An argument the analysis types wider than its parameter is checked as the parameter's type, which PHP checks where
// the argument lands and throws a `TypeError` for any other value (spec section 21).
final class Drawing
{
    public function draw(Shape $shape): string
    {
        return $this->square($shape);
    }

    public function describe(Shape $shape): string
    {
        return Square::describe(square: $shape);
    }

    public function title(): string
    {
        return $this->shout($this->translated('title'));
    }

    /** @return array<string, string>|string */
    public function translated(string $key): array|string
    {
        return $key;
    }

    private function square(Square $square): string
    {
        return 'square';
    }

    private function shout(string $text): string
    {
        return $text;
    }
}
