<?php

declare(strict_types=1);

namespace App;

// PHP# writes an empty `List` `[]` and an empty `Map` `[:]`, so `[]` where either lands picks neither (spec section 12).
final class Either
{
    /** @var list<int>|array<string, int> */
    private array $values = [];
}
