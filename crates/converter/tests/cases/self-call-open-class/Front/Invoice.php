<?php

declare(strict_types=1);

namespace Front;

use App\Tax;

// Code outside `App` calls both methods, so they stay public, and a subclass may replace either.
final class Invoice
{
    public function due(int $amount): int
    {
        return Tax::total($amount) + Tax::rate($amount);
    }
}
