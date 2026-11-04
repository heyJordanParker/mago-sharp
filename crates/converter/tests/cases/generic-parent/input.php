<?php

declare(strict_types=1);

namespace App;

/**
 * A parent's type arguments, and a trait's, join the header (spec section 11).
 *
 * @template T
 *
 * @extends Box<list<T>>
 * @implements Holds<list<T>>
 */
final class Shelf extends Box implements Holds
{
    /** @use Counts<T> */
    use Counts;

    /** @return list<T> */
    public function held(): array
    {
        return $this->item;
    }
}
