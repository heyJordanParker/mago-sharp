<?php

declare(strict_types=1);

namespace App;

// In a `final` class no subclass can be the class PHP called, so `self::label()` is `Badge.label()`.
final class Badge
{
    public const string LABEL = 'badge';

    public static function describe(): string
    {
        return self::label();
    }

    public static function label(): string
    {
        return self::LABEL;
    }
}
