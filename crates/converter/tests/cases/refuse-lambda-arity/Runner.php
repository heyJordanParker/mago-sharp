<?php

declare(strict_types=1);

namespace App;

final class Runner
{
    /** @param \Closure(Runner, int &$count): void $callback */
    public function run(\Closure $callback): void
    {
        $count = 0;
        $callback($this, $count);
    }
}
