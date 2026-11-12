<?php

declare(strict_types=1);

namespace App;

/**
 * A model whose columns only PHPDoc types, as a framework's model, which may hold `1` for a `bool` and `"0"` for an
 * `int`.
 *
 * @property bool $active
 * @property int $count
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
