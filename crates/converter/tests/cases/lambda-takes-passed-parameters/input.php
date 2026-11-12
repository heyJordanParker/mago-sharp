<?php

declare(strict_types=1);

namespace App;

// PHP calls a lambda with more arguments than it takes, and a PHP# lambda takes every parameter its function type
// passes (spec section 14.1), so the lambda gains each one it ignores, named `unused` where the type names none.
final class Report
{
    /** @return list<string> */
    public function marked(Lines $lines): array
    {
        return $lines->map(fn (string $line): string => $line . '!');
    }

    public function touch(Lines $lines): void
    {
        $lines->each(function (): void {
        });
    }
}
