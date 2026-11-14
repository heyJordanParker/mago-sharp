<?php

declare(strict_types=1);

namespace App;

// `__FUNCTION__` and `__METHOD__` outside a function, as in a property's default, give nothing, and in a property's
// accessor the accessor's name, as `$label::get` (spec section 6).
final class Named
{
    public string $where = __FUNCTION__ . __METHOD__;

    public string $label {
        get => __FUNCTION__;
    }
}
