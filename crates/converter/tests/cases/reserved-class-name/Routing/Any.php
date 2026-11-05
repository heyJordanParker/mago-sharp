<?php

declare(strict_types=1);

namespace Lib\Routing;

use Attribute;

#[Attribute(Attribute::TARGET_METHOD)]
final class Any
{
    public function __construct(public string $path)
    {
    }
}
