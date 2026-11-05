<?php

declare(strict_types=1);

namespace App;

/**
 * A bound of any value, `null` included, bounds nothing, which PHP# writes with no bound (spec section 11).
 *
 * @template TFirst of mixed|null
 * @template TSecond of mixed
 */
final class Pair
{
    /**
     * @param TFirst $first
     * @param TSecond $second
     */
    public function __construct(public readonly mixed $first, public readonly mixed $second)
    {
    }
}
