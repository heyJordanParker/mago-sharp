<?php

declare(strict_types=1);

namespace App\Support;

final class Container
{
    /**
     * @template T of object
     *
     * @param class-string<T> $class
     *
     * @return T
     */
    public static function make(string $class): object
    {
        return new $class();
    }
}
