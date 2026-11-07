<?php

declare(strict_types=1);

namespace App;

// A PHP# type holds one collection, a `List`, a `Set` or a `Map` (spec section 12), and a PHP# `Iterable` reads values
// only, where PHP's `iterable<K, V>` keeps the keys it gives.
final class Shapes
{
    /** @var list<string>|array<string, int> */
    public array $either = [];

    /** @return iterable<string, int> */
    public function counts(): iterable
    {
        return [];
    }

    /** @return iterable<int> */
    public function values(): iterable
    {
        return [];
    }
}
