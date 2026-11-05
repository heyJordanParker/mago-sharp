<?php

declare(strict_types=1);

namespace Proof;

// The refinement worker types special and plain, and takes make()'s return from its body, which is inferred.
#[Refined]
class Factory
{
    public $special;

    public $plain;

    public function make()
    {
        return new Box(new Spec('made'));
    }
}
