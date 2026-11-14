<?php

declare(strict_types=1);

namespace App;

// A public member of an application names its types. A method that declares `mixed`, or no return type, returns what
// its body returns, which names them, and `null` where the body runs on past its last statement.
final class Store
{
    public function label(): mixed
    {
        return 'store';
    }

    public function counted(bool $any)
    {
        if ($any) {
            return 1;
        }
    }
}
