<?php

declare(strict_types=1);

namespace App\Routing;

use Attribute;
use Sharp\DynamicallyAccessed;

// A router reaches each method this attribute marks by reflection.
#[Attribute(Attribute::TARGET_METHOD)]
#[DynamicallyAccessed]
final class Route
{
    public function __construct(public string $path)
    {
    }
}
