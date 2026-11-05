<?php

declare(strict_types=1);

namespace App;

use Countable;

// An open method is `virtual`, one that replaces a parent class's is `override`, and a `final` or private one is
// neither (spec section 22).
class SalesReport extends Report implements Titled, Countable
{
    public function title(): string
    {
        return $this->heading(text: 'Sales');
    }

    public function count(): int
    {
        return 1;
    }

    final public function total(): int
    {
        return 2;
    }

    private function heading(string $text): string
    {
        return $text;
    }
}
