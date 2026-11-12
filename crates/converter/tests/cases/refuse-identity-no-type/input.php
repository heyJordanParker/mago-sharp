<?php

declare(strict_types=1);

namespace App;

// `===` of a string and a class value refuses, since a PHP# class value equals only a class value, and so does `===`
// of a value PHP# has no type for, as a resource or an `array{…}` shape.
final class Uploads
{
    /** @param class-string $handler */
    public function handles(string $name, string $handler): bool
    {
        return $name === $handler;
    }

    /**
     * @param resource $stream
     * @param resource $other
     */
    public function same($stream, $other): bool
    {
        return $stream === $other;
    }

    /** @param array{width: int, height: int}|false $size */
    public function measured(array|false $size): bool
    {
        return $size !== false;
    }
}
