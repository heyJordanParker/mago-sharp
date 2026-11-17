<?php

declare(strict_types=1);

namespace App;

// A `(bool)` of a declared `bool`, a condition of a call read once, a local typed by an operator of declared values,
// and a `return` whose value first assigns a local (spec sections 2, 3, 21 and 24).
final class Forms
{
    private ?int $count = null;

    /** @param bool $on */
    public function flag($on): bool
    {
        return (bool) $on;
    }

    public function counted(): ?int
    {
        return $this->count;
    }

    public function shown(): bool
    {
        return $this->counted() && true;
    }

    public function hidden(): bool
    {
        return !$this->counted();
    }

    public function computed(bool $wide, int $count): int
    {
        if ($wide) {
            $size = $count * 2;
        } else {
            $size = 1;
        }

        return $size;
    }

    public function inside(?string $text): string
    {
        return ($found = $text) !== null ? $found : '';
    }
}
