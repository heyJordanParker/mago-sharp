<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Protocol;

use function chr;
use function count;
use function pack;
use function strlen;

/**
 * Packs the fields of one string mago reads whole, such as a method search, in the byte layout mago's PayloadReader
 * reads: big-endian integers, a byte for a boolean, and a string after its 32-bit length.
 *
 * @internal
 */
final class PayloadWriter
{
    private string $payload = '';

    public function writeU8(int $value): void
    {
        $this->payload .= chr($value);
    }

    public function writeU32(int $value): void
    {
        $this->payload .= pack('N', $value);
    }

    public function writeBoolean(bool $value): void
    {
        $this->payload .= $value ? "\1" : "\0";
    }

    /**
     * @param array<array-key, mixed> $values
     */
    public function writeCount(array $values): void
    {
        $this->payload .= pack('N', count($values));
    }

    public function writeBytes(string $value): void
    {
        $this->payload .= pack('N', strlen($value)) . $value;
    }

    public function finish(): string
    {
        return $this->payload;
    }
}
