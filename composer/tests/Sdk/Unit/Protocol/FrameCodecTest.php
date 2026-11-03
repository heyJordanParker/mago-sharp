<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Protocol;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Io\ResourceReader;
use Mago\Sdk\Internal\Protocol\Frame;
use Mago\Sdk\Internal\Protocol\FrameCodec;
use Mago\Tests\Sdk\Unit\StreamPair;
use PHPUnit\Framework\TestCase;

use function fclose;
use function fwrite;
use function Mago\Internal\get_version;

final class FrameCodecTest extends TestCase
{
    public function testResponseFrameRoundTrips(): void
    {
        $codec = new FrameCodec(1024);
        $encoded = $codec->encode(Frame::response(42, "payload\0"));
        [$input, $output] = StreamPair::create();
        fwrite($output, $encoded);
        fclose($output);
        $reader = new ResourceReader($input);
        $frame = $codec->read($reader);
        $reader->close();
        fclose($input);

        self::assertNotNull($frame);
        self::assertSame(42, $frame->id);
        self::assertSame("payload\0", $frame->payload);
    }

    /**
     * A frame of another protocol names both protocols and the fix, as a request of another protocol does.
     */
    public function testAFrameOfAnotherProtocolNamesBothProtocolsAndTheFix(): void
    {
        $codec = new FrameCodec();
        $encoded = $codec->encode(Frame::response(42, ''));
        // The header's minor version, bytes 6 and 7, becomes 1.
        $encoded[7] = "\1";
        [$input, $output] = StreamPair::create();
        fwrite($output, $encoded);
        fclose($output);
        $reader = new ResourceReader($input);

        $this->expectExceptionObject(new ProtocolException(
            '`mago` speaks extension frame protocol 1.1, and the extension SDK, heyjordanparker/mago-sharp '
            . get_version()
            . ', speaks 1.0. `mago` and heyjordanparker/mago-sharp must be the same release: run the `mago` that '
            . 'heyjordanparker/mago-sharp installs, `vendor/bin/mago`, or require the heyjordanparker/mago-sharp '
            . 'release `mago --version` prints.',
        ));

        try {
            $codec->read($reader);
        } finally {
            $reader->close();
            fclose($input);
        }
    }
}
