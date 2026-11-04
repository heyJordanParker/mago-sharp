<?php

declare(strict_types=1);

namespace App;

// A method named as a value, `name(...)`, `[$this, 'name']` or `'Class::name'` where a function is declared, is
// PHP#'s method value `this.name` (spec section 14.3).
final class Pricing
{
    /** @param \Closure(int): int $rule */
    public function apply(\Closure $rule, int $amount): int
    {
        return $rule($amount);
    }

    /** @param callable(int): int $rule */
    public function run(callable $rule, int $amount): int
    {
        return $rule($amount);
    }

    public function discounted(int $amount): int
    {
        return $amount - 1;
    }

    public static function doubled(int $amount): int
    {
        return $amount * 2;
    }

    public function total(int $amount): int
    {
        $first = $this->apply($this->discounted(...), $amount);
        $second = $this->apply(self::doubled(...), $first);
        $third = $this->run([$this, 'discounted'], $second);

        return $this->run('App\Pricing::doubled', $third);
    }
}
