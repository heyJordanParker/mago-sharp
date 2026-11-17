<?php

declare(strict_types=1);

namespace App;

final class Node
{
    public function __construct(public int $value, public ?Node $next = null)
    {
    }
}
