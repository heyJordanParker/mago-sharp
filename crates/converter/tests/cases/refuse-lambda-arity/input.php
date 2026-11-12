<?php

declare(strict_types=1);

namespace App;

// PHP calls a lambda with more arguments than it takes, and a PHP# lambda takes every parameter its function type
// passes (spec section 14.1). A PHP# parameter is never taken by reference, so a lambda whose function type passes
// one that way has no PHP# form.
final class Caller
{
    public function call(Runner $runner): void
    {
        $runner->run(function (): void {
        });
    }
}
