<?php

declare(strict_types=1);

namespace App;

// A library's function and constant in PHP's root namespace are reached through the namespace of the classes beside
// them, which one `extern namespace` line the conversion adds gives their folder (spec section 23).
final class Orders
{
    public function missing(): void
    {
        abort(404);
    }

    public function version(): string
    {
        return ACME_VERSION;
    }
}
