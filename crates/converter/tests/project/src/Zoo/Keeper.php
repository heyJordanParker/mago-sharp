<?php

declare(strict_types=1);

namespace Proof\Zoo;

// Plain PHP of the animals' namespace that sets their properties, which a PHP# property's `set` reaches, where code of
// another namespace would keep each class plain PHP.
final class Keeper
{
    public function rename(Bird $bird, string $sound, string $name): void
    {
        $bird->sound = $sound;
        $bird->name = $name;
    }
}
