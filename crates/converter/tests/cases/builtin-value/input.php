<?php

declare(strict_types=1);

namespace App;

// A built-in as a value is the lambda that calls the standard library member that runs it, with as many parameters as
// the function type it lands in passes, since PHP calls the built-in with each of them (spec section 14.2).
final class Rules
{
    /** @param callable(string): string $rule */
    public function apply(callable $rule, string $text): string
    {
        return $rule($text);
    }

    /** @param callable(string, string): bool $rule */
    public function test(callable $rule, string $text): bool
    {
        return $rule($text, 'a');
    }

    public function cleaned(string $text): string
    {
        return $this->apply(mb_trim(...), $text);
    }

    public function prefixed(string $text): bool
    {
        return $this->test(str_starts_with(...), $text);
    }
}
