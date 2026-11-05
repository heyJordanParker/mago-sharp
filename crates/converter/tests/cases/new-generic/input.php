<?php

declare(strict_types=1);

namespace Proof\Shop;

use Proof\Box;
use Proof\Spec;
use Proof\Special;

// Each `new Box` takes the type arguments of the declared type its value goes into, or Box's bound.
final class Crate
{
    /** @var Box<Spec> */
    private Box $box;

    public function __construct(Spec $spec)
    {
        $this->box = new Box($spec);
    }

    public function open(): Spec
    {
        return $this->box->get();
    }

    /** @return Box<Special> */
    public function special(): Box
    {
        return new Box(new Special('crated'));
    }

    public function loose(): object
    {
        return new Box(new Spec('loose'));
    }
}
