<?php

declare(strict_types=1);

namespace Proof\Shop;

use Proof\Spec;

// A subclass whose constructor starts with its parent's, which `: super(…)` runs once it is converted.
class Tagged extends Spec
{
    public function __construct(string $name, public string $tag)
    {
        parent::__construct($name);
    }

    public function label(): string
    {
        return $this->tag . ' ' . parent::label();
    }
}
