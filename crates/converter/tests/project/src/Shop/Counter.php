<?php

declare(strict_types=1);

namespace Proof\Shop;

final class Counter
{
    public static int $step_size = 2;

    public function bump(int &$count): void
    {
        $count++;
    }

    // Only PHPDoc types the result, as a framework's model types a column, so it may hold `1` or `"abc"`.
    /** @return bool */
    public function enabled(mixed $value)
    {
        return $value;
    }
}
