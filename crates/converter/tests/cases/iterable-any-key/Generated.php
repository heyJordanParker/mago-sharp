<?php

declare(strict_types=1);

namespace App;

// A plain PHP implementer whose generator gives string keys, which a plain PHP keyed loop over the converted `Feed`
// reads as before: the PHP# interface leaves the generator PHP gives untouched.
final class Generated implements Feed
{
    /** @return iterable<string> */
    public function items(): iterable
    {
        yield 'first' => 'a';
        yield 'second' => 'b';
    }

    /** @return iterable<array-key, int> */
    public function counts(): iterable
    {
        yield 'one' => 1;
    }

    public function shown(): string
    {
        $shown = '';
        foreach ($this->items() as $key => $value) {
            $shown .= $key . '=' . $value . ' ';
        }

        return $shown;
    }
}
