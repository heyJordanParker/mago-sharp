<?php

declare(strict_types=1);

namespace Proof\Shop;

final class Pair
{
    public function both(): string
    {
        return PairHelper::join('left', 'right');
    }
}

final class PairHelper
{
    public static function join(string $left, string $right): string
    {
        return $left . '+' . $right;
    }
}
