<?php

declare(strict_types=1);

namespace App;

final class Stamp
{
    public int $copies = 0;

    public function __clone()
    {
        $this->copies++;
    }
}
