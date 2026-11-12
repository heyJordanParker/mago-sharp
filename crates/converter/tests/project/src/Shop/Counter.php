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
}
