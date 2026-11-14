<?php

declare(strict_types=1);

namespace App;

// `never` is a whole type, which no value has: a method's return, and a type argument, as a list that holds no
// element (spec section 7).
final class Failure
{
    public function fail(string $reason): never
    {
        throw new \RuntimeException($reason);
    }

    /** @return list<never> */
    public function none(): array
    {
        return [];
    }
}
