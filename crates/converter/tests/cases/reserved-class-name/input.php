<?php

declare(strict_types=1);

namespace App;

use Lib\Payments\Error;
use Lib\Routing\Any;

// PHP# reserves `Any` for a type and `Error` for its interface of every failure, so a library's class of either name
// is imported under its namespace's last part before it (spec sections 11 and 31).
final class Controller
{
    #[Any('/ping')]
    public function ping(): string
    {
        return 'pong';
    }

    public function declined(): Error
    {
        return new Error('declined');
    }
}
