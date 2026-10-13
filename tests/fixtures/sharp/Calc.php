<?php

declare(strict_types=1);

namespace Lib;

final class Calc
{
    public static function make(): self
    {
        return new self();
    }

    public function add(int $a, int $b): int
    {
        return $a + $b;
    }
}
