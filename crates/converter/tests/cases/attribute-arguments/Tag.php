<?php

declare(strict_types=1);

namespace App;

use Attribute;

#[Attribute(Attribute::TARGET_PARAMETER | Attribute::TARGET_PROPERTY | Attribute::IS_REPEATABLE)]
class Tag
{
    public const Text = 'text';

    /** @param list<string|Tag> $anyOf */
    public function __construct(public array $anyOf = [], public ?string $list = null)
    {
    }
}
