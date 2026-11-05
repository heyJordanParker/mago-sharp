<?php

declare(strict_types=1);

namespace App;

// A PHP# local is declared where it is first assigned and lives in its block (spec sections 2 and 3), so a local
// that starts with no type, starts inside another expression, or is a loop's variable read after the loop keeps the
// file plain PHP.
final class Locals
{
    /** @return list<int> */
    public function empty(): array
    {
        $items = [];

        return $items;
    }

    public function inside(?string $text): string
    {
        if (($found = $text) !== null) {
            return $found;
        }

        return '';
    }

    /** @param list<int> $items */
    public function after(array $items): int
    {
        foreach ($items as $item) {
        }

        return $item;
    }

    // A local read after the block that assigns it is declared first in the method's body with its type, which only a
    // declaration, a PHPDoc tag, a refinement or an inherited type of each value assigned to it gives (decision 2).
    public function computed(bool $wide, int $count): int
    {
        if ($wide) {
            $size = $count * 2;
        } else {
            $size = 1;
        }

        return $size;
    }

    public function guessed(bool $wide): int
    {
        if ($wide) {
            $size = $this->guess();
            $copy = $size;
        } else {
            $copy = 1;
        }

        return $copy;
    }

    private function guess()
    {
        return 2;
    }
}
