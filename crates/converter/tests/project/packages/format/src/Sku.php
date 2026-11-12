<?php

declare(strict_types=1);

namespace Acme\Format;

// A plain PHP value object that PHP shows as text through `__toString`.
final class Sku
{
    public function __construct(private string $code)
    {
    }

    public function __toString(): string
    {
        return 'SKU-' . $this->code;
    }
}
