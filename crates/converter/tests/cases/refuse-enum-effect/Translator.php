<?php

declare(strict_types=1);

namespace App\Support;

final class Translator
{
    public static function get(string $key): string
    {
        return $key;
    }

    public static function upper(string $text): string
    {
        return strtoupper($text);
    }
}
