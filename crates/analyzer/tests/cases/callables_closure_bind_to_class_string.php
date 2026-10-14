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
 * @param class-string<Counter> $scope
 *
 * @throws Error
 */
function increment(string $scope): void
{
    (\Closure::bind(
        static function (): void {
            Counter::$count++;
        },
        null,
        $scope,
    ) ?? throw new Error('failed to bind the closure'))();
}
