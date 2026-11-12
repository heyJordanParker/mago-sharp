<?php

declare(strict_types=1);

namespace App;

use LogicException;
use RuntimeException;

// A `catch` of several classes takes their union (spec section 31).
final class Guard
{
    public function run(): string
    {
        try {
            return 'ok';
        } catch (RuntimeException|LogicException $e) {
            return $e->getMessage();
        }
    }
}
