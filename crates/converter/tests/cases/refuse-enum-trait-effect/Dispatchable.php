<?php

declare(strict_types=1);

namespace App\Bus;

trait Dispatchable
{
    public static function dispatch(mixed ...$arguments): void
    {
    }
}
