<?php

declare(strict_types=1);

namespace App;

// A subclass's constructor calls its parent's with `: super(…)`, which a later slice writes (spec section 9).
class Tagged extends Spec
{
    public function __construct(string $name, public string $tag)
    {
        parent::__construct($name);
    }
}
