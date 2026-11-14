<?php

declare(strict_types=1);

namespace App;

// A `static` local keeps its value from one call of its method to the next, as a private static field does in PHP#,
// whose type joins the values written to it (spec section 6).
final class Ticker
{
    public function next(): int
    {
        static $count = 0;

        return ++$count;
    }

    /** @return list<string> */
    public function seen(string $name): array
    {
        static $names = [];
        $names[] = $name;

        return $names;
    }

    // A member of the class takes `next`, so the field takes its method's name before its own.
    public function last(int $value): ?int
    {
        static $next;
        $previous = $next;
        $next = $value;

        return $previous;
    }
}
