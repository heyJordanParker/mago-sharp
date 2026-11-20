<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Protocol;

use Mago\Sdk\Internal\Protocol\PayloadWriter;
use PHPUnit\Framework\TestCase;

final class PayloadTest extends TestCase
{
    /** Each field is packed in the byte layout mago's PayloadReader reads. */
    public function testEachFieldPacksAsMagoReadsIt(): void
    {
        $writer = new PayloadWriter();
        $writer->writeU8(7);
        $writer->writeU32(0x0405_0607);
        $writer->writeBoolean(true);
        $writer->writeBoolean(false);
        $writer->writeCount(['a', 'b']);
        $writer->writeBytes("by\0tes");

        self::assertSame("\x07\x04\x05\x06\x07\x01\x00\x00\x00\x00\x02\x00\x00\x00\x06by\0tes", $writer->finish());
    }
}
