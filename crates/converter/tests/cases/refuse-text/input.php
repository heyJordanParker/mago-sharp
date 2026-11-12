<?php

declare(strict_types=1);

namespace App;

// A template shows a `bool` as `true` or `false` (spec section 18). The conversion moves lines, so `__LINE__` would
// give another line than the PHP file's, and so would `__FUNCTION__` in a lambda, whose PHP name holds its line.
final class Texts
{
    public function run(bool $ready): string
    {
        $shown = "ready: $ready";
        $method = fn (): string => __FUNCTION__;
        $line = __LINE__;

        return $shown . $method() . $line;
    }
}
