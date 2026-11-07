<?php

declare(strict_types=1);

namespace App;

final class Runner
{
    /** @param \Closure(Runner): void $callback */
    public function run(\Closure $callback): void
    {
        $callback($this);
    }
}
