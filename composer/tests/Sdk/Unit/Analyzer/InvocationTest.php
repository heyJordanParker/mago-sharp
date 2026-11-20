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
use Mago\Sdk\Internal\Protocol\ValueWriter;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;
use Throwable;

use function array_slice;

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

        self::assertSame([0, 0], array_slice($response, 3));
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
        self::decode(self::message($writer->finish()));
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
        self::decode(self::message($writer->finish()));
    }

    public function testTruncatedReceiverIsRejected(): void
    {
        $writer = self::requestPrefix(2);
        $writer->writeBytes('BaseModel');
        $writer->writeBytes('target');
        self::writeReceiver($writer, Type::namedObject('User'));

        $this->expectException(Throwable::class);
        self::decode(self::message(array_slice($writer->finish(), 0, -1)));
    }

    public function testEmptyMethodDeclaringClassIsRejected(): void
    {
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage('Analyzer invocation names cannot be empty.');

        self::decode(self::request(2, '', Type::namedObject('User')));
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

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private static function request(
        int $kind,
        ?string $declaringClass = null,
        ?Type $receiver = null,
        ?FunctionLikeIdentifier $callingFunctionLike = null,
    ): array {
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

        return self::message($writer->finish());
    }

    private static function requestPrefix(int $kind): ValueWriter
    {
        $writer = new ValueWriter();
        $writer->writeU64(1);
        $writer->writeU8($kind);
        $writer->writeU16(1);
        $writer->writeU16(0);

        return $writer;
    }

    private static function writeReceiver(ValueWriter $writer, Type $receiver): void
    {
        $writer->writeU32(0);
        TypeCodec::writeComplete($writer, $receiver);
    }

    /**
     * The return-type request of `$fields`.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $fields
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private static function message(array $fields): array
    {
        return [Protocol::NAME, Protocol::RETURN_TYPE_REQUEST, ...$fields];
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     */
    private static function decode(array $payload): ReturnTypeRequest
    {
        [, $reader] = Protocol::readRequest($payload);

        return Protocol::readReturnTypeRequest($reader);
    }
}
