<?php

declare(strict_types=1);

final class Circle
{
}

final class Square
{
}

/**
 * @return class-string<($round is true ? Circle : Square)>
 */
function shape_class(bool $round): string
{
    return $round ? Circle::class : Square::class;
}

function make_circle(): Circle
{
    $class = shape_class(true);

    return new $class();
}
