<?php

declare(strict_types=1);

namespace App;

// A local whose first value says nothing of what it holds takes the type its `@var` tag declares.
final class Tally
{
    /** @return list<string> */
    public function names(): array
    {
        /** @var list<string> $names */
        $names = [];
        $names[] = 'a';

        return $names;
    }

    /** @return array<string, int> */
    public function counts(): array
    {
        /** @var array<string, int> $counts */
        $counts = [];
        $counts['a'] = 1;

        return $counts;
    }

    public function first(): ?string
    {
        /** @var ?string $first */
        $first = null;

        return $first;
    }
}
