<?php

declare(strict_types=1);

namespace App;

use Attribute;
use Sharp\DynamicallyAccessed;

// An attribute class a framework reads by reflection keeps its mark, so the members it marks are never too wide (spec
// section 5, decision 95). Plain PHP marks it as `#[\Sharp\DynamicallyAccessed]`.
#[Attribute(Attribute::TARGET_METHOD)]
#[DynamicallyAccessed]
final class Action
{
    public function __construct(public string $name)
    {
    }
}
