<?php

declare(strict_types=1);

namespace App;

// PHP's `iterable<V>` keys by any `array-key`, which says no more than a PHP# `Iterable<V>`, which reads values only
// (spec section 12).
interface Feed
{
    /** @return iterable<string> */
    public function items(): iterable;

    /** @return iterable<array-key, int> */
    public function counts(): iterable;
}
