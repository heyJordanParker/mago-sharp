<?php

declare(strict_types=1);

namespace App;

// `switch` waits for a later family, `echo` for the `Console` a class receives, and `goto`, `if … endif`, a loop by
// reference or by count, and `unset` of a variable have no PHP# form (spec sections 8, 17 and 21).
final class Statements
{
    /** @param list<int> $items */
    public function loops(array $items, int $count): void
    {
        foreach ($items as &$item) {
        }
        foreach ($count as $item) {
        }
        while (true) {
            while (true) {
                break 2;
            }
        }
        $name = 'a';
        foreach ($items as $name) {
        }
    }

    public function choose(bool $flag): void
    {
        switch ($flag) {
            case true:
                break;
        }
    }

    public function remove(int $count): void
    {
        unset($count);
        echo 'done';
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
