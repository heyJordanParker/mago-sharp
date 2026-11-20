<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Protocol;

use function count;

/**
 * Writes the fields of a message to mago, each one value of its list, in the order the field's codec gives them.
 *
 * @internal
 */
final class ValueWriter
{
    /**
     * @param list<null|bool|int|float|string|list<mixed>> $values
     */
    public function __construct(
        private array $values = [],
    ) {}

    public function writeU8(int $value): void
    {
        $this->values[] = $value;
    }

    public function writeU16(int $value): void
    {
        $this->values[] = $value;
    }

    public function writeU32(int $value): void
    {
        $this->values[] = $value;
    }

    public function writeU64(int $value): void
    {
        $this->values[] = $value;
    }

    public function writeI64(int $value): void
    {
        $this->values[] = $value;
    }

    public function writeF64(float $value): void
    {
        $this->values[] = $value;
    }

    public function writeBoolean(bool $value): void
    {
        $this->values[] = $value;
    }

    /**
     * @param array<array-key, mixed> $values
     */
    public function writeCount(array $values): void
    {
        $this->values[] = count($values);
    }

    public function writeLength(int $length): void
    {
        $this->values[] = $length;
    }

    public function writeBytes(string $value): void
    {
        $this->values[] = $value;
    }

    public function writeString(string $value): void
    {
        $this->values[] = $value;
    }

    public function writeOptionalString(?string $value): void
    {
        $this->values[] = $value;
    }

    /**
     * Writes the fields another writer finished, in order, as fields of this message.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $values
     */
    public function writeValues(array $values): void
    {
        foreach ($values as $value) {
            $this->values[] = $value;
        }
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public function finish(): array
    {
        return $this->values;
    }
}
