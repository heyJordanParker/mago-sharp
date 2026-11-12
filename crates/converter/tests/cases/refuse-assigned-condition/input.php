<?php

declare(strict_types=1);

namespace App;

// An `else if` that assigns the local it tests refuses, since its assignment runs only when the first test fails, and
// a line of its own before the `if` would run it every time.
final class Lookup
{
    public function find(string $key): ?string
    {
        return $key === '' ? null : $key;
    }

    public function first(string $key, string $other): string
    {
        if ($key === 'none') {
            return '';
        } elseif ($found = $this->find($other)) {
            return $found;
        }

        return $key;
    }
}
