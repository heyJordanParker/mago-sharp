<?php

declare(strict_types=1);

namespace App;

final class Finder
{
    /**
     * @template T of object
     * @param class-string<T> $class
     * @return T|null
     */
    public function find(string $class): ?object
    {
        return null;
    }
}
