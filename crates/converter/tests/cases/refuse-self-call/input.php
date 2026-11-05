<?php

declare(strict_types=1);

namespace App;

// `self::label()` forwards the class PHP called, which `static::LABEL` inside `label` reads, so `Square::describe()`
// gives a subclass's label. A call through the class's name doesn't forward it, so in a class a subclass can extend
// the call, and its method value, have no PHP# form.
class Shape
{
    public const string LABEL = 'shape';

    public static function describe(): string
    {
        return self::label();
    }

    /** @return \Closure(): string */
    public static function labeller(): \Closure
    {
        return self::label(...);
    }

    public static function label(): string
    {
        return static::LABEL;
    }
}
