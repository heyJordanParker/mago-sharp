<?php

declare(strict_types=1);

namespace App;

// A method with no PHP return type returns `Any?` in PHP#, so a local assigned its call is an `Any?`, read after its
// block or appended to a `[]` (decision 2).
final class Sizes
{
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

    public function listed(): int
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
