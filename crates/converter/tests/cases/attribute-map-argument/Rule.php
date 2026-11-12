<?php

declare(strict_types=1);

namespace App;

use Attribute;

#[Attribute(Attribute::TARGET_PROPERTY | Attribute::TARGET_PARAMETER | Attribute::IS_REPEATABLE)]
final class Rule
{
    public function __construct(public array $anyOf = [], public ?Rule $of = null)
    {
    }
}
