<?php

declare(strict_types=1);

namespace App;

use ArrayIterator;
use IteratorAggregate;

// A plain PHP collection, as a framework's is, which PHP loops over through its iterator.
/** @implements IteratorAggregate<int, Line> */
final class Lines implements IteratorAggregate
{
    /** @param list<Line> $lines */
    public function __construct(private array $lines)
    {
    }

    /** @return ArrayIterator<int, Line> */
    public function getIterator(): ArrayIterator
    {
        return new ArrayIterator($this->lines);
    }
}
