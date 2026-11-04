<?php

declare(strict_types=1);

namespace App;

use LogicException;
use RuntimeException;

// A `catch` of several types waits for a later slice.
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
