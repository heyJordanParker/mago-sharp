<?php

declare(strict_types=1);

namespace Proof;

// Stays plain PHP: a subclass of a converted class, which must still link against it.
class Special extends Spec
{
    public function label(): string
    {
        return 'special ' . $this->name;
    }
}
