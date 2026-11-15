<?php

declare(strict_types=1);

namespace App;

// A `(bool)` of a declared `bool`, a condition of a call read once, a local typed by an operator of declared values,
// and a `return` whose value first assigns a local (spec sections 2, 3, 21 and 24).
final class Forms
{
    private string $name = '';

    /** @param bool $on */
    public function flag($on): bool
    {
        return (bool) $on;
    }

    public function named(): string
    {
        return $this->name;
    }

    public function shown(): bool
    {
        return $this->named() && true;
    }

    public function hidden(): bool
    {
        return !$this->named();
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
