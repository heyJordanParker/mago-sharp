<?php

declare(strict_types=1);

namespace Acme\Format;

/**
 * A row whose attributes only PHPDoc types, as a framework's model does, and which holds what its storage gave it, as
 * `"5"` from a database, or `5` where PHPDoc says `string`.
 *
 * @property int $quantity
 * @property string $code
 * @property ?string $note
 */
final class Record
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
