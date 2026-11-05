<?php

declare(strict_types=1);

namespace Lib\Payments;

final class Error
{
    public function __construct(public string $reason)
    {
    }
}
