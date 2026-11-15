<?php

declare(strict_types=1);

namespace App\Shop;

final class Ticker extends Counter
{
    public function tick(): void
    {
        $this->count = $this->count + 1;
    }
}
