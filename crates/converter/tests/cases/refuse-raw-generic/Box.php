<?php

declare(strict_types=1);

namespace App;

/** @template T */
final class Box
{
    /** @param T $item */
    public function __construct(public mixed $item)
    {
    }
}
