<?php

declare(strict_types=1);

namespace App;

use function Acme\Pdf\render;
use const Acme\Pdf\SIZE;

// A library's function and constant are reached through their namespace, and one in PHP's root namespace through
// the namespace its `extern namespace` line gives it, so `use function` and `use const` leave no line (spec section 23).
final class Printer
{
    public function loud(string $text): string
    {
        return shout($text);
    }

    public function page(string $html): string
    {
        return render($html);
    }

    public function size(): int
    {
        return SIZE;
    }
}
