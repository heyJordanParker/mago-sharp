<?php

declare(strict_types=1);

namespace App;

use Closure;
use RuntimeException as Failure;

// A `use` line of a class of PHP's root namespace goes, since the file names each such class by its own name: a
// `Closure` as the function type it is, and a root class of `Builtins.sharp` bare (spec sections 14.1 and 23).
final class Pipeline
{
    /** @param Closure(int): int $next */
    public function run(Closure $next, int $value): int
    {
        if ($value < 0) {
            throw new Failure('negative');
        }

        return $next($value);
    }
}
