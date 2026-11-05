<?php

declare(strict_types=1);

namespace App;

// A plain PHP trait the class uses joins its header (spec section 22).
final class Host
{
    use Greets;

    public function welcome(): string
    {
        return $this->greet() . '!';
    }
}
