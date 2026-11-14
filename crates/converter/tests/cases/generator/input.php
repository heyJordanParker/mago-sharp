<?php

declare(strict_types=1);

namespace App;

// A method with `yield` returns an `Iterable` of what it yields, and `yield from` is `yield ...` (spec section 12).
final class Lines
{
    /** @return \Generator<int, string> */
    public function lines(): \Generator
    {
        yield 'a';
        yield from $this->more();
    }

    /** @return iterable<string> */
    private function more(): iterable
    {
        yield 'b';
    }
}
