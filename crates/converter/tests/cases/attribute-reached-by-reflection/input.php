<?php

declare(strict_types=1);

namespace App;

use Attribute;
use Sharp\ReachedByReflection;

// An attribute class a framework reads by reflection keeps its mark, so the members it marks never warn that they are
// too wide (spec section 5, decision 95). Plain PHP marks it as `#[\Sharp\ReachedByReflection]`.
#[Attribute(Attribute::TARGET_METHOD)]
#[ReachedByReflection]
final class Action
{
    public function __construct(public string $name)
    {
    }
}
