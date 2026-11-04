<?php

declare(strict_types=1);

namespace App;

// Loops, `switch`, `unset` and a body without braces wait for later slices, `echo` waits for the `Console` a slice
// injects, and `goto` and `if … endif` have no PHP# form (spec sections 8, 17 and 21).
final class Statements
{
    /** @param list<int> $items */
    public function loops(array $items, bool $flag): void
    {
        foreach ($items as $item) {
        }
        while ($flag) {
        }
    }

    public function choose(bool $flag): void
    {
        switch ($flag) {
            case true:
                break;
        }
    }

    public function remove(): void
    {
        /** @var array<string, int> $map */
        $map = ['a' => 1];
        unset($map['a']);
        echo 'done';
    }

    public function unbraced(bool $flag): void
    {
        if ($flag) return;
    }

    public function alternative(bool $flag): void
    {
        if ($flag):
        endif;
    }

    public function jump(): void
    {
        goto end;
        end:
    }
}
