<?php

declare(strict_types=1);

namespace Proof;

// The refinement worker gives Box the type parameter T of Spec, its item the type T, and get() the return T.
#[Refined]
class Box
{
    public function __construct(public $item)
    {
    }

    public function get()
    {
        return $this->item;
    }
}
