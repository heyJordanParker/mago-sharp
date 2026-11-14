<?php

declare(strict_types=1);

namespace App;

// A method a subclass may replace keeps the return PHP declares, since the subclass may return the wider value, so an
// open method of an application that declares `mixed`, or no return, refuses (spec section 22).
class Shelf
{
    public function label(): mixed
    {
        return 'shelf';
    }

    public function counted()
    {
        return 1;
    }
}
