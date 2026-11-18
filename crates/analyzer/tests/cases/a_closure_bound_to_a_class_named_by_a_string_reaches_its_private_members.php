<?php

declare(strict_types=1);

final class Counter
{
    private static int $count = 0;

    public static function count(): int
    {
        return self::$count;
    }
}

/**
 * @throws Error
 */
function increment(): void
{
    (\Closure::bind(
        static function (): void {
            Counter::$count++;
        },
        null,
        'Counter',
    ) ?? throw new Error('failed to bind the closure'))();
}
