<?php

declare(strict_types=1);

namespace App;

// `(bool)`, `(array)` and `(object)` have no PHP# form (spec section 24).
final class Casts
{
    public function run(int $value): void
    {
        $flag = (bool) $value;
        $list = (array) $value;
        $object = (object) $value;
    }
}
