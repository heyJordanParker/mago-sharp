<?php

declare(strict_types=1);

namespace App;

// A template shows a `bool` as `true` or `false`, and `Position` names no class or method PHP's way (spec sections 18
// and 27). The conversion moves lines, so `__LINE__` would give another line than the PHP file's.
final class Texts
{
    public function run(bool $ready): string
    {
        $shown = "ready: $ready";
        $class = __CLASS__;
        $method = __METHOD__;
        $line = __LINE__;

        return $shown . $method . $line;
    }
}
