<?php

declare(strict_types=1);

namespace App;

/**
 * @template T
 */
interface Holds
{
    /** @return T */
    public function held();
}
