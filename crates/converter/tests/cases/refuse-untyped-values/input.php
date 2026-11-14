<?php

declare(strict_types=1);

namespace App;

// A value the analysis gives `mixed` or `object` reaches a typed property and a member only in PHP: PHP# takes it
// there from a known type (spec section 24).
final class Untyped
{
    private ?string $last = null;

    public function keep(mixed $value): void
    {
        $this->last = $value;
    }

    public function name(object $value): string
    {
        return $value->name();
    }
}
