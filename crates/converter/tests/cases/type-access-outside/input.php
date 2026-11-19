<?php

namespace App;

// In a project, a type that code outside its namespace uses stays `public`, as the checker's `too-wide-member` reads
// its uses (spec section 5). `Page`, of `Web`, uses `Basket`.
final class Basket
{
    public function total(): int
    {
        return 10;
    }
}
