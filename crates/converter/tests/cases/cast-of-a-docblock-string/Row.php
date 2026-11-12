<?php

declare(strict_types=1);

namespace App;

// A model whose attributes PHPDoc types, as a framework's `@property` does, and which holds whatever its storage gave
// it at runtime, as `5` from a database.
/**
 * @property string $label
 * @property ?string $note
 */
final class Row
{
    /** @param array<string, mixed> $attributes */
    public function __construct(private array $attributes)
    {
    }

    public function __get(string $name): mixed
    {
        return $this->attributes[$name] ?? null;
    }
}
