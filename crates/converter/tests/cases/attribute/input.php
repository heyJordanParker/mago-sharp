<?php

declare(strict_types=1);

namespace App;

use Attribute;
use Lib\Route;

#[Attribute(Attribute::TARGET_CLASS)]
final class Endpoint
{
    #[Route('/orders', name: 'orders')]
    public function orders(#[Route('/one')] int $one): int
    {
        return $one;
    }
}
