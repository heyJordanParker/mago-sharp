<?php

declare(strict_types=1);

namespace App;

// A local that starts as `[]` and is both appended to and written by key, or that starts as `[]` or `null` and is
// written a value no declaration gives the type of, keeps the file plain PHP (decision 2).
final class Mixed
{
    public function both(string $key): int
    {
        $values = [];
        $values[] = 1;
        $values[$key] = 2;

        return count($values);
    }

    public function guessed(): int
    {
        $values = [];
        $values[] = $this->guess();

        return count($values);
    }

    public function nulled(bool $wide): int
    {
        $size = null;
        if ($wide) {
            $size = $this->guess();
        }

        return $size ?? 0;
    }

    private function guess()
    {
        return 2;
    }
}
