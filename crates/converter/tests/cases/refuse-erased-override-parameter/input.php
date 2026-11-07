<?php

declare(strict_types=1);

namespace App;

// An untyped parameter takes the type the PHPDoc of the method it replaces gives it, as `Request` here. PHP# writes
// that type in the PHP it compiles to, and PHP links an override only when it takes at least what the replaced
// method's PHP declaration takes, which is any value (spec section 22).
final class Page implements Responder
{
    public function respond($request): string
    {
        return $request->path;
    }
}
