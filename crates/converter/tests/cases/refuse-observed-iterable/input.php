<?php

declare(strict_types=1);

namespace App;

// A PHP# `Iterable<V>` parameter takes a new sequence of the values it is given, numbered from 0, so an `iterable`
// parameter converts only where the body loops over its values alone. A keyed loop, a return, a call that passes it
// on, `instanceof` and a method call refuse, in a method or a lambda, and so does a constructor that stores it as a
// property, which every other method may read (spec section 12).
final class Feeds
{
    /** @param iterable<string> $stored */
    public function __construct(private iterable $stored)
    {
    }

    /** @param iterable<string> $items */
    public function keyed(iterable $items): string
    {
        $shown = '';
        foreach ($items as $key => $item) {
            $shown .= $key . $item;
        }

        return $shown;
    }

    /**
     * @param iterable<string> $items
     * @return iterable<string>
     */
    public function through(iterable $items): iterable
    {
        return $items;
    }

    /** @param iterable<string> $items */
    public function joined(iterable $items): string
    {
        $shown = '';
        foreach ($items as $item) {
            $shown .= $item;
        }

        return $shown;
    }

    /** @return \Closure(iterable): iterable */
    public function passer(): \Closure
    {
        return static fn (iterable $items): iterable => $items;
    }
}
