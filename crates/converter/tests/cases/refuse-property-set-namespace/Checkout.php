<?php

declare(strict_types=1);

namespace App\Shop;

use App\Billing\Transaction;

final class Checkout
{
    public function pay(Transaction $transaction): void
    {
        $transaction->status = 'paid';
        $transaction->total = 0;
    }
}
