<?php

declare(strict_types=1);

namespace App;

/**
 * PHP# names a type parameter `T`, or `T` and a capital letter, so `Item` keeps the class plain PHP (spec section 11).
 *
 * @template Item
 */
final class Crate
{
    /** @param Item $item */
    public function __construct(public readonly mixed $item)
    {
    }
}
