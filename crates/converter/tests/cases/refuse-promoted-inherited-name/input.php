<?php

declare(strict_types=1);

namespace App;

// A promoted property named as a method the class inherits clashes with it, as a declared property does (spec section
// 14).
final class Named extends Base
{
    public function __construct(
        public string $title,
        public ?string $name = null,
    ) {
    }
}
