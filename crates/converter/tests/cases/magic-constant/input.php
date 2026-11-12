<?php

declare(strict_types=1);

namespace App;

// `__DIR__` and `__FILE__` are what `Position.current()` gives (spec section 27), and `__CLASS__` is `self::class`, the
// class value (spec section 25). `__METHOD__`, `__FUNCTION__`, `__NAMESPACE__` and `__TRAIT__` are the text PHP
// compiles in their place, which the class's own declarations fix, in a lambda too for the namespace.
final class Paths
{
    public function stubs(): string
    {
        $file = __FILE__;

        return __DIR__ . '/stubs:' . $file;
    }

    public function traced(): string
    {
        $named = fn (): string => 'in ' . __NAMESPACE__;

        return __METHOD__ . ' ' . __FUNCTION__ . ' ' . $named() . __TRAIT__;
    }

    /** @return class-string<self> */
    public function kind(): string
    {
        return __CLASS__;
    }
}
