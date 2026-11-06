<?php

declare(strict_types=1);

namespace Proof\Shop;

// Stays plain PHP: a PHP# `Iterable<V>` parameter takes a new sequence of the values it is given, numbered from 0, so
// the `iterable` it returns would have other keys and another class.
final class Feed
{
    /**
     * @param iterable<string> $items
     * @return iterable<string>
     */
    public function through(iterable $items): iterable
    {
        return $items;
    }
}
