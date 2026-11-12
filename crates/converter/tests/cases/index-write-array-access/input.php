<?php

declare(strict_types=1);

namespace App;

// `=` at an index of an `ArrayAccess` object, as a framework's collection or a model, calls its `offsetSet` both ways,
// and `unset` at one calls its `offsetUnset`.
final class Checkout
{
    public function add(Order $order, string $sku, int $quantity): void
    {
        $order[$sku] = $quantity;
    }

    public function drop(Order $order, string $sku): void
    {
        unset($order[$sku]);
    }
}
