<?php

declare(strict_types=1);

namespace App;

// `new static(…)` is `new Self(…)`, which makes an object of the class PHP called. A class that isn't `final` marks its
// constructor `required`, which `new Self(…)` calls on it and on every subclass.
class Money
{
    public function __construct(public readonly int $cents)
    {
    }

    public function doubled(): static
    {
        return new static($this->cents * 2);
    }
}
