<?php

declare(strict_types=1);

namespace App;

// An `if` that tests the value it assigns is the assignment on its own line and the test of the local, since a PHP#
// assignment is a statement. An `else if` that assigns refuses, since its assignment runs only when the first test
// fails, and `refuse-assigned-condition` keeps that.
final class Orders
{
    /** @var array<string, Order> */
    private array $orders = [];

    public function find(string $reference): ?Order
    {
        return $this->orders[$reference] ?? null;
    }

    public function total(string $reference, string $fallback): int
    {
        if ($order = $this->find($reference)) {
            return $order->total;
        }
        if ($order = $this->find($fallback)) {
            return $order->total;
        } else {
            return 0;
        }
    }
}
