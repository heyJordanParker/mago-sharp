<?php

declare(strict_types=1);

namespace Lib;

use Attribute;

#[Attribute(Attribute::TARGET_ALL)]
final class Route
{
    public function __construct(public string $path, public string $name = '')
    {
    }
}
