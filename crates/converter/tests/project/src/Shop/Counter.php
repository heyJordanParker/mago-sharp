<?php

declare(strict_types=1);

namespace Proof\Shop;

final class Counter
{
    public function bump(int &$count): void
    {
        $count++;
    }
}
