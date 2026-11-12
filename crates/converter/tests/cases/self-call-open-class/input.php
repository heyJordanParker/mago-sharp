<?php

declare(strict_types=1);

namespace App;

// In a class a subclass can extend, `self::rate()` is `Tax.rate()` where `rate`, and each method it calls through
// `self`, names neither `static` nor `parent`, so no late static binding reads the class PHP called.
class Tax
{
    public static function total(int $amount): int
    {
        return $amount + self::rate($amount);
    }

    public static function rate(int $amount): int
    {
        return self::percent() * $amount;
    }

    private static function percent(): int
    {
        return 2;
    }
}
