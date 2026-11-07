<?php

declare(strict_types=1);

namespace App;

final class Hooks
{
    /** @param (\Closure(int): int)|(\Closure(): int) $callback */
    public function add(\Closure $callback): void
    {
    }
}
