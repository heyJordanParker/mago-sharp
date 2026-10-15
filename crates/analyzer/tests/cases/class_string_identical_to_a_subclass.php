<?php

declare(strict_types=1);

abstract class Shape
{
}

final class Ring extends Shape
{
}

/**
 * @param class-string<Shape> $class
 *
 * @return class-string<Ring>|null
 */
function ring_or_null(string $class): ?string
{
    if ($class === Ring::class) {
        return $class;
    }

    return null;
}
