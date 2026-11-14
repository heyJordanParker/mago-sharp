<?php

declare(strict_types=1);

namespace App;

// A plain PHP trait an enum uses joins its header, after its backing type and interfaces (spec section 22).
enum Tier: string implements Sized
{
    use Labels;

    case Basic = 'basic';
}
