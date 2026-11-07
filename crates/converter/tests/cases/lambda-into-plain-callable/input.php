<?php

declare(strict_types=1);

namespace App;

// A lambda that takes fewer parameters than a function type passes refuses, counting each parameter with no default
// and no `...`, as the PHP# checker counts them (spec section 14.1). A plain `callable` takes `mixed ...`, so it
// passes none, and a lambda of no parameters goes where one is declared.
final class Setup
{
    public function run(Hooks $hooks): void
    {
        $hooks->add('expiry', fn () => 60);
    }
}
