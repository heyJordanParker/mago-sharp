<?php

declare(strict_types=1);

namespace App;

/**
 * @template T
 */
trait Counts
{
    /** @param list<T> $items */
    public function count(array $items): int
    {
        return \count($items);
    }
}
