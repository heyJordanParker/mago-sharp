<?php

declare(strict_types=1);

namespace App;

// PHP leaves a typed property with no initial value unset until code outside the class sets it, as a framework that
// hydrates an entity does. PHP# sets every property before the object is used (spec section 6).
final class Ticket
{
    public string $title;

    public ?int $count;

    public string $code;

    public function __construct()
    {
        $this->code = 'new';
    }
}
