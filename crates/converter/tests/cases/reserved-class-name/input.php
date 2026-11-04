<?php

declare(strict_types=1);

namespace App;

use Lib\Routing\Any;

// PHP# reserves `Any` for a type, so a library's class of that name is imported under its namespace's last part before
// it (spec section 11).
final class Controller
{
    #[Any('/ping')]
    public function ping(): string
    {
        return 'pong';
    }
}
