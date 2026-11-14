<?php

declare(strict_types=1);

namespace App;

// PHP# refuses a `?` on a return that no subclass can replace and that never gives `null` (spec section 14.4), so such
// a return drops it, as every value the method returns has the narrower type. A method a subclass may replace keeps
// its `?`.
class Kinds
{
    final public function kind(): ?string
    {
        return 'kind';
    }

    public function open(): ?string
    {
        return 'open';
    }

    private function hidden(int $value): ?int
    {
        return $value + 1;
    }

    public function shown(): ?int
    {
        return $this->hidden(1);
    }
}
