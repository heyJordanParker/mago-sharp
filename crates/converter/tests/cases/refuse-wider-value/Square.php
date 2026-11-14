<?php

declare(strict_types=1);

namespace App;

final class Square implements Shape
{
    public static function describe(?Square $square): string
    {
        return 'square';
    }
}
