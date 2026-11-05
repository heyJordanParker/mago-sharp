<?php

declare(strict_types=1);

namespace App;

// `static fn` and `static function` are lambdas too. A static closure's body names no `$this`, so each call of the
// lambda gives the result PHP's gives (spec section 14.2).
final class Pricing
{
    /** @return \Closure(int): int */
    public function taxed(int $rate): \Closure
    {
        return static fn(int $amount): int => $amount + $amount * $rate;
    }

    /** @return \Closure(int): int */
    public function discounted(int $off): \Closure
    {
        return static function (int $amount) use ($off): int {
            return $amount - $off;
        };
    }
}
