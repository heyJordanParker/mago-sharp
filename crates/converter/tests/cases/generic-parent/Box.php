<?php

declare(strict_types=1);

namespace App;

/**
 * @template T
 */
class Box
{
    /** @param T $item */
    public function __construct(public $item)
    {
    }
}
