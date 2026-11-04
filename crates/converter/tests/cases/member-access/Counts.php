<?php

declare(strict_types=1);

namespace App;

final class Counts
{
    public const int LIMIT = 3;

    public static int $total = 0;

    public static function zero(): int
    {
        return 0;
    }
}
