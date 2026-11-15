<?php

declare(strict_types=1);

namespace App;

class Order
{
    public const int MAX = 10;

    public const LIMIT = 5;

    public static int $count = 0;

    public static function find(int $id): ?Order
    {
        return null;
    }
}
