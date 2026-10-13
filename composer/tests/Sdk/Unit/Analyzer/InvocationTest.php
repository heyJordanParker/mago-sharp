<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use Mago\Sdk\Analyzer\InvocationKind;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Analyzer\Type\FunctionLikeIdentifier;
use Mago\Sdk\Analyzer\Type\FunctionLikeKind;
use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Analyzer\Protocol;
use Mago\Sdk\Internal\Analyzer\ReturnTypeRequest;
use Mago\Sdk\Internal\Analyzer\TypeCodec;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;
use Throwable;

use function array_diff;
use function pack;
use function range;
use function sprintf;
use function substr;

/**
 * @mago-expect lint:too-many-methods
 */
final class InvocationTest extends TestCase
{
    /**
     * @return iterable<string, array{int, InvocationKind, null|string, null|string}>
     */
    public static function validInvocations(): iterable
    {
        yield 'function' => [1, InvocationKind::Function, null, null];
        yield 'instance method' => [2, InvocationKind::InstanceMethod, 'BaseModel', 'User'];
        yield 'static method' => [3, InvocationKind::StaticMethod, 'BaseModel', 'User'];
    }

    #[DataProvider('validInvocations')]
    public function testInvocationContextRoundTrips(
        int $encodedKind,
        InvocationKind $kind,
        ?string $declaringClass,
        ?string $receiver,
    ): void {
        $request = self::decode(self::request(
            $encodedKind,
            $declaringClass,
            $receiver === null ? null : Type::namedObject($receiver),
        ));

        self::assertSame($kind, $request->invocation->kind);
        self::assertSame('target', $request->invocation->name);
        self::assertSame($declaringClass, $request->invocation->declaringClass);
        self::assertSame(
            $receiver,
            $request->invocation->receiverType === null ? null : (string) $request->invocation->receiverType,
        );
    }

    public function testCallingFunctionLikeRoundTrips(): void
    {
        $calling = new FunctionLikeIdentifier(FunctionLikeKind::Method, 'subject', 'Remark');
        $request = self::decode(self::request(2, 'BaseModel', Type::namedObject('User'), $calling));

        self::assertNotNull($request->invocation->callingFunctionLike);
        self::assertTrue($calling->equals($request->invocation->callingFunctionLike));
        self::assertNull(self::decode(self::request(1))->invocation->callingFunctionLike);
    }

    public function testUnknownInvocationKindIsRejected(): void
    {
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage('Unknown analyzer invocation kind 255.');

        self::decode(self::request(255));
    }

    public function testReceiverRetainsItsSnapshotHandle(): void
    {
        $request = self::decode(self::request(2, 'BaseModel', Type::namedObject('User')));
        $receiver = $request->invocation->receiverType;
        self::assertNotNull($receiver);

        $response = Protocol::writeReturnTypeResponse($receiver);

        self::assertSame(pack('CN', 0, 0), substr($response, 13));
    }

    public function testMethodWithoutReceiverIsRejected(): void
    {
        $writer = self::requestPrefix(2);
        $writer->writeBytes('BaseModel');
        $writer->writeBytes('target');
        $writer->writeU32(1);
        $writer->writeU32(2);
        $writer->writeU16(0);

        $this->expectException(Throwable::class);
        self::decode(self::message($writer));
    }

    public function testFunctionWithReceiverIsRejected(): void
    {
        $writer = self::requestPrefix(1);
        $writer->writeBytes('target');
        self::writeReceiver($writer, Type::namedObject('Unexpected'));
        $writer->writeU32(1);
        $writer->writeU32(2);
        $writer->writeU16(0);

        $this->expectException(Throwable::class);
        self::decode(self::message($writer));
    }

    public function testTruncatedReceiverIsRejected(): void
    {
        $writer = self::requestPrefix(2);
        $writer->writeBytes('BaseModel');
        $writer->writeBytes('target');
        self::writeReceiver($writer, Type::namedObject('User'));

        $this->expectException(Throwable::class);
        self::decode(self::messagePayload(substr($writer->finish(), 0, -3) . "\x02"));
    }

    public function testEmptyMethodDeclaringClassIsRejected(): void
    {
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage('Analyzer invocation names cannot be empty.');

        self::decode(self::request(2, '', Type::namedObject('User')));
    }

    /**
     * @return iterable<string, array{int, int}>
     */
    public static function requestsFromOtherVersions(): iterable
    {
        $currentMinor = Protocol::VERSION_U32 & 0xFFFF;
        foreach (array_diff(range(0, $currentMinor + 1), [$currentMinor]) as $minor) {
            yield "return type, minor {$minor}" => [Protocol::RETURN_TYPE_REQUEST, $minor];
            yield "callable signature, minor {$minor}" => [Protocol::CALLABLE_SIGNATURE_REQUEST, $minor];
            yield "assertion, minor {$minor}" => [Protocol::ASSERTION_REQUEST, $minor];
        }
    }

    #[DataProvider('requestsFromOtherVersions')]
    public function testRequestFromAnotherVersionIsRejected(int $kind, int $minor): void
    {
        $major = Protocol::VERSION_U32 >> 16;
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage(sprintf('Unsupported analyzer protocol version %d.%d.', $major, $minor));

        Protocol::readRequest(pack('N3', 0x4D41_4E41, ($major << 16) | $minor, $kind << 16));
    }

    /**
     * @return iterable<string, array{string, int}>
     */
    public static function unhandledResponses(): iterable
    {
        yield 'return type' => [Protocol::writeReturnTypeResponse(null), 0x8002];
        yield 'callable signature' => [Protocol::writeCallableSignatureResponse(null), 0x800C];
        yield 'assertion' => [Protocol::writeAssertionResponse(null), 0x8012];
    }

    #[DataProvider('unhandledResponses')]
    public function testUnhandledResponseCarriesTheCurrentVersion(string $response, int $kind): void
    {
        [$decodedKind, $reader] = Protocol::readRequest($response);

        self::assertSame($kind, $decodedKind);
        self::assertFalse($reader->readBoolean());
        $reader->finish();
    }

    private static function request(
        int $kind,
        ?string $declaringClass = null,
        ?Type $receiver = null,
        ?FunctionLikeIdentifier $callingFunctionLike = null,
    ): string {
        $writer = self::requestPrefix($kind);
        if ($declaringClass !== null) {
            $writer->writeBytes($declaringClass);
        }
        $writer->writeBytes('target');
        if ($receiver !== null) {
            self::writeReceiver($writer, $receiver);
        }
        $writer->writeU32(1);
        $writer->writeU32(2);
        $writer->writeU16(0);
        $writer->writeBoolean($callingFunctionLike !== null);
        if ($callingFunctionLike !== null) {
            TypeCodec::writeFunctionLikeIdentifier($writer, $callingFunctionLike);
        }

        return self::message($writer);
    }

    private static function requestPrefix(int $kind): PayloadWriter
    {
        $writer = new PayloadWriter();
        $writer->writeU64(1);
        $writer->writeU8($kind);
        $writer->writeU16(1);
        $writer->writeU16(0);

        return $writer;
    }

    private static function writeReceiver(PayloadWriter $writer, Type $receiver): void
    {
        $writer->writeU32(0);
        TypeCodec::writeComplete($writer, $receiver);
    }

    private static function message(PayloadWriter $writer): string
    {
        return self::messagePayload($writer->finish());
    }

    private static function messagePayload(string $payload): string
    {
        return pack('N3', 0x4D41_4E41, Protocol::VERSION_U32, 2 << 16) . $payload;
    }

    private static function decode(string $payload): ReturnTypeRequest
    {
        [, $reader] = Protocol::readRequest($payload);

        return Protocol::readReturnTypeRequest($reader);
    }
}
