<?php

declare(strict_types=1);

namespace Acme\Format;

final class Formatter
{
    public function wrap(string $text): string
    {
        return '[' . $text . ']';
    }
}
