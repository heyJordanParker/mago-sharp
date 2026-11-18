<?php

declare(strict_types=1);

class Shape
{
    public function area(): int
    {
        return 0;
    }
}

final class Square extends Shape
{
    public function largest(Shape|string $other): int
    {
        if ($other instanceof parent) {
            return $other->area();
        }

        return 0;
    }
}
