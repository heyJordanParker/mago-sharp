<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Protocol;

use Mago\Sdk\Internal\Io\ResourceReader;
use Mago\Sdk\Internal\Protocol\Frame;
use Mago\Sdk\Internal\Protocol\FrameCodec;
use Mago\Tests\Sdk\Unit\StreamPair;
use PHPUnit\Framework\TestCase;

use function fclose;
use function fwrite;

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
}
