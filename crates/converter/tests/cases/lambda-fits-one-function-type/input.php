<?php

declare(strict_types=1);

namespace App;

// A lambda goes where one of the function types it may land in passes no more parameters than it takes, as the PHP#
// checker decides it (spec section 14.1).
final class Setup
{
    public function run(Hooks $hooks): void
    {
        $hooks->add(fn () => 60);
    }
}
