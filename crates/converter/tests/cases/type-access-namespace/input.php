<?php

namespace App;

// In a project, a type that nothing outside its namespace uses is `internal`, as the checker's `too-wide-member` reads
// its uses (spec section 5). Only `Checkout`, of `App`, uses `Basket`.
final class Basket
{
    public function total(): int
    {
        return 10;
    }
}
