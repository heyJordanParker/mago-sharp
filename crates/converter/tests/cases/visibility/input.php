<?php

declare(strict_types=1);

namespace App;

// PHP reaches every class and every member written without a visibility, so PHP# writes `public` (spec section 5).
class Legacy
{
    var $note;

    function describe(): string
    {
        return 'legacy';
    }

    protected function hidden(): int
    {
        return 1;
    }

    private function own(): int
    {
        return 2;
    }
}
