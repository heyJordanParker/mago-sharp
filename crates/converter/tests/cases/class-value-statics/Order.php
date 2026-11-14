<?php

declare(strict_types=1);

namespace App;

class Order
{
    public const int MAX = 10;

    public static int $count = 0;

    public function __construct(public int $id)
    {
    }

    public static function find(int $id): ?Order
    {
        return $id > 0 ? new Order($id) : null;
    }

    public static function label(int $id): string
    {
        return 'order ' . $id;
    }
}
