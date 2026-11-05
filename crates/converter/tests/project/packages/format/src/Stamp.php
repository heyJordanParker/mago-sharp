<?php

declare(strict_types=1);

namespace Acme\Format;

final class Stamp
{
    public int $copies = 0;

    public function __clone()
    {
        $this->copies++;
    }
}
