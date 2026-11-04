<?php

declare(strict_types=1);

namespace App;

// Static methods and properties keep `static`, a class constant writes its type, and PHP's `private(set)` is the
// `set` accessor's own visibility (spec sections 2, 6 and 11).
class Counter
{
    public const int LIMIT = 10;

    private const PREFIX = 'count';

    private static int $count = 0;

    public private(set) int $value = 0;

    public function __construct(public private(set) string $label)
    {
        self::$count++;
    }

    public static function made(): int
    {
        return self::$count;
    }

    public function step(): string
    {
        if ($this->value < self::LIMIT) {
            $this->value++;
        }

        return self::PREFIX . ' ' . $this->label;
    }
}
