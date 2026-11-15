<?php

declare(strict_types=1);

// A file's one namespace in braces is `namespace App;`, which a PHP# file's first line names (spec section 27).
namespace App {
    use App\Shop\Item;

    final class Braced
    {
        public function name(Item $item): string
        {
            return $item->name;
        }
    }
}
