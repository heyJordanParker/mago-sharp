<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Analyzer\Protocol;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class InvocationTest extends TestCase
{
    /** A return-type request is the object mago's engine builds, so a message that holds no such object is refused. */
    public function testARequestThatHoldsNoRequestObjectIsRejected(): void
    {
        [, $reader] = Protocol::readRequest([Protocol::NAME, Protocol::RETURN_TYPE_REQUEST, 1, 2, 0]);

        $this->expectException(ProtocolException::class);

        Protocol::readReturnTypeRequest($reader);
    }

    /** A message whose first value names another protocol is no analyzer message, so the worker refuses it. */
    public function testAMessageOfAnotherProtocolIsRejected(): void
    {
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage('The message is no analyzer message.');

        Protocol::readRequest(['linter', Protocol::DESCRIBE_REQUEST]);
    }

    /**
     * @return iterable<string, array{list<null|bool|int|float|string|list<mixed>>, int}>
     */
    public static function unhandledResponses(): iterable
    {
        yield 'return type' => [Protocol::writeReturnTypeResponse(null), 0x8002];
        yield 'callable signature' => [Protocol::writeCallableSignatureResponse(null), 0x800C];
        yield 'assertion' => [Protocol::writeAssertionResponse(null), 0x8012];
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $response
     */
    #[DataProvider('unhandledResponses')]
    public function testUnhandledResponseSaysItHandledNothing(array $response, int $kind): void
    {
        [$decodedKind, $reader] = Protocol::readRequest($response);

        self::assertSame($kind, $decodedKind);
        self::assertFalse($reader->readBoolean());
        $reader->finish();
    }
}
