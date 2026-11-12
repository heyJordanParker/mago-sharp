<?php

declare(strict_types=1);

namespace App;

// `(array)` and `(object)` have no PHP# form, nor `(bool)` of a value the analysis types `bool`, which PHPDoc alone may
// say (spec section 24).
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
