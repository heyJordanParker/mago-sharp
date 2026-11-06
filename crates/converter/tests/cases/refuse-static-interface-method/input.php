<?php

declare(strict_types=1);

namespace App;

// A PHP# interface declares instance members, and `static` alone before an operator (spec section 22), so an
// interface's static method has no PHP# form.
interface Counted
{
    public static function limit(): int;

    public function consumes(): int;
}
