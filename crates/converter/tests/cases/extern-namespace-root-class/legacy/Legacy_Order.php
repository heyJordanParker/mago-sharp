<?php

declare(strict_types=1);

// A class in PHP's root namespace, in a library whose classes have no namespace.
final class Legacy_Order
{
    public function __construct(
        public string $reference = '',
    ) {}
}
