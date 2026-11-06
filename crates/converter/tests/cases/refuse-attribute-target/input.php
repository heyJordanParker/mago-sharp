<?php

declare(strict_types=1);

namespace App;

// PHP checks where an attribute goes only when reflection creates it, and PHP# when it compiles (spec section 1). An
// enum case is a class constant to an attribute's targets.
enum Kind: string
{
    #[Label('Text')]
    case Text = 'text';

    #[Label('Name')]
    public function caption(): string
    {
        return $this->value;
    }
}
