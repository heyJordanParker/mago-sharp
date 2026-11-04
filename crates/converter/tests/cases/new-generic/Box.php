<?php

declare(strict_types=1);

namespace Proof;

/**
 * @template T of Spec
 */
class Box
{
    /** @param T $item */
    public function __construct(public $item)
    {
    }

    /** @return T */
    public function get()
    {
        return $this->item;
    }
}
