<?php

declare(strict_types=1);

namespace App;

// PHP changes a function's own copy of an array argument, and a PHP# parameter never changes, so a function that
// changes its parameters copies each one into a `let` local first, which the rest of the body uses (spec section 2).
final class Signup
{
    /**
     * @param array<string, string> $input
     * @param list<string> $form
     */
    public function fields(array $input, array $form, string $key): int
    {
        $input[$key] = 'signup';
        $form[] = 'email';

        return count($input) + count($form);
    }
}
