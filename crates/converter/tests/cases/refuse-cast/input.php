<?php

declare(strict_types=1);

namespace App;

// `(array)` and `(object)` have no PHP# form, and `(bool)` of a `bool` a declaration makes one at runtime, as a
// parameter, is the value itself (spec section 24).
final class Casts
{
    /** @param bool $on */
    public function run(int $value, $on): void
    {
        $flag = (bool) $on;
        $list = (array) $value;
        $object = (object) $value;
    }
}
