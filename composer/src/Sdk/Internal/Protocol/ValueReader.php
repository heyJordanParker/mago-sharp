<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Protocol;

use Mago\Sdk\Exception\ProtocolException;

use function count;
use function get_debug_type;
use function is_array;
use function is_bool;
use function is_float;
use function is_int;
use function is_string;

/**
 * Reads the fields of a message mago sent, each one value of its list, in the order the field's codec gives them.
 *
 * @internal
 */
final class ValueReader
{
    /**
     * @var non-negative-int
     */
    private int $position;

    /**
     * @var non-negative-int
     */
    private readonly int $count;

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $values
     * @param non-negative-int $position
     */
    public function __construct(
        private readonly array $values,
        int $position = 0,
    ) {
        $this->position = $position;
        $this->count = count($values);
    }

    /**
     * @return int<0, 255>
     */
    public function readU8(): int
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_int($value) || $value < 0 || $value > 0xFF) {
            throw self::unexpected('an integer from 0 to 255', $value);
        }

        return $value;
    }

    /**
     * @return int<0, 65535>
     */
    public function readU16(): int
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_int($value) || $value < 0 || $value > 0xFFFF) {
            throw self::unexpected('an integer from 0 to 65535', $value);
        }

        return $value;
    }

    /**
     * @return int<0, 4294967295>
     */
    public function readU32(): int
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_int($value) || $value < 0 || $value > 0xFFFF_FFFF) {
            throw self::unexpected('an integer from 0 to 4294967295', $value);
        }

        return $value;
    }

    /**
     * @return int<0, max>
     */
    public function readU64(): int
    {
        $value = $this->readI64();
        if ($value < 0) {
            throw new ProtocolException('An unsigned protocol integer exceeds the largest integer supported by PHP.');
        }

        return $value;
    }

    public function readI64(): int
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_int($value)) {
            throw self::unexpected('an integer', $value);
        }

        return $value;
    }

    public function readF64(): float
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_float($value)) {
            throw self::unexpected('a float', $value);
        }

        return $value;
    }

    /**
     * @param int<0, max> $count
     *
     * @return list<int<0, 4294967295>>
     */
    public function readU32List(int $count): array
    {
        $values = [];
        for ($index = 0; $index < $count; ++$index) {
            $values[] = $this->readU32();
        }

        return $values;
    }

    public function readBoolean(): bool
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_bool($value)) {
            throw self::unexpected('a boolean', $value);
        }

        return $value;
    }

    /**
     * @param int<0, 4294967295> $maximum
     *
     * @return int<0, 4294967295>
     */
    public function readCount(int $maximum): int
    {
        $count = $this->readU32();
        if ($count > $maximum) {
            throw new ProtocolException("A protocol collection count {$count} exceeds the limit of {$maximum}.");
        }

        return $count;
    }

    public function readBytes(): string
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_string($value)) {
            throw self::unexpected('a string', $value);
        }

        return $value;
    }

    public function readString(): string
    {
        return $this->readBytes();
    }

    public function readOptionalString(): ?string
    {
        if ($this->position < $this->count && $this->values[$this->position] === null) {
            ++$this->position;

            return null;
        }

        return $this->readBytes();
    }

    /**
     * Reads a field that holds a list of its own, such as a message inside this one.
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public function readList(): array
    {
        $value = $this->values[$this->position++] ?? null;
        if (!is_array($value)) {
            throw self::unexpected('a list', $value);
        }

        /** @var list<null|bool|int|float|string|list<mixed>> $value mago's engine sends no other values. */
        return $value;
    }

    public function finish(): void
    {
        if ($this->position !== $this->count) {
            throw new ProtocolException('A protocol payload was not consumed exactly.');
        }
    }

    private static function unexpected(string $expected, mixed $value): ProtocolException
    {
        $sent = $value === null ? 'none' : get_debug_type($value);

        return new ProtocolException("A protocol field holds {$expected}, but mago sent {$sent}.");
    }
}
