<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Worker;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Protocol\ValueReader;
use Mago\Sdk\Internal\Protocol\ValueWriter;

use function count;

/**
 * @internal
 */
final class Protocol
{
    /** The name every worker management message starts with. */
    public const NAME = 'worker';

    public const COLLECT_REQUEST = 1;
    public const REDUCE_REQUEST = 2;
    public const CLASS_LOADS_REQUEST = 3;

    private const COLLECT_RESPONSE = 0x8001;
    private const REDUCE_RESPONSE = 0x8002;
    private const CLASS_LOADS_RESPONSE = 0x8003;
    private const MAXIMUM_WORKERS = 0x0001_0000;
    private const MAXIMUM_REDUCERS = 0x0000_4000;
    private const MAXIMUM_CLASSES = 0x0010_0000;

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{int<0, 65535>, ValueReader}
     */
    public static function readRequest(array $payload): array
    {
        [$kind, $reader] = self::readMessage($payload);
        if ($kind !== self::COLLECT_REQUEST && $kind !== self::REDUCE_REQUEST && $kind !== self::CLASS_LOADS_REQUEST) {
            throw new ProtocolException("Unknown worker management request kind {$kind}.");
        }

        return [$kind, $reader];
    }

    /**
     * Reads the classes a class loads request asks after, each a lowercase class name.
     *
     * @return list<string>
     */
    public static function readClassLoadsRequest(ValueReader $reader): array
    {
        $classes = [];
        for ($count = $reader->readCount(self::MAXIMUM_CLASSES); $count > 0; --$count) {
            $classes[] = $reader->readBytes();
        }

        $reader->finish();

        return $classes;
    }

    /**
     * Answers whether the worker's class loads hold one of the classes asked after.
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeClassLoadsResponse(bool $holds): array
    {
        $writer = self::createMessage(self::CLASS_LOADS_RESPONSE);
        $writer->writeBoolean($holds);

        return $writer->finish();
    }

    /**
     * @param array<int<0, max>, string> $payloadsByExtension
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeCollectResponse(array $payloadsByExtension): array
    {
        $writer = self::createMessage(self::COLLECT_RESPONSE);
        $writer->writeCount($payloadsByExtension);
        foreach ($payloadsByExtension as $extensionIndex => $payload) {
            $writer->writeU32($extensionIndex);
            $writer->writeBytes($payload);
        }

        return $writer->finish();
    }

    /**
     * @param list<int<0, max>> $reducerIndices
     *
     * @return array<int<0, max>, non-empty-list<string>>
     */
    public static function readReduceRequest(ValueReader $reader, array $reducerIndices): array
    {
        $workerCount = $reader->readCount(self::MAXIMUM_WORKERS);
        if ($workerCount === 0) {
            throw new ProtocolException('A worker reduction request contains no worker payloads.');
        }

        $payloadsByExtension = [];
        for ($workerIndex = 0; $workerIndex < $workerCount; ++$workerIndex) {
            $workerPayloads = self::readCollectResponse($reader->readList(), $reducerIndices);
            foreach ($workerPayloads as $extensionIndex => $payload) {
                $payloadsByExtension[$extensionIndex][] = $payload;
            }
        }

        $reader->finish();

        return $payloadsByExtension;
    }

    /**
     * Answers a reduce request with `$issues`, the issues the reducers reported, as the analyzer protocol writes them.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $issues
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    public static function writeReduceResponse(array $issues): array
    {
        return (new ValueWriter([self::NAME, self::REDUCE_RESPONSE, $issues]))->finish();
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     * @param list<int<0, max>> $reducerIndices
     *
     * @return array<int<0, max>, string>
     */
    private static function readCollectResponse(array $payload, array $reducerIndices): array
    {
        [$kind, $reader] = self::readMessage($payload);
        if ($kind !== self::COLLECT_RESPONSE) {
            throw new ProtocolException("Expected worker collection response, received message kind {$kind}.");
        }

        $count = $reader->readCount(self::MAXIMUM_REDUCERS);
        if ($count !== count($reducerIndices)) {
            throw new ProtocolException('Workers advertised inconsistent reducer registrations.');
        }

        $payloads = [];
        for ($index = 0; $index < $count; ++$index) {
            $extensionIndex = $reader->readU32();
            if ($extensionIndex !== $reducerIndices[$index]) {
                throw new ProtocolException('Workers advertised inconsistent reducer registrations.');
            }

            $payloads[$extensionIndex] = $reader->readBytes();
        }

        $reader->finish();

        return $payloads;
    }

    /**
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return array{int<0, 65535>, ValueReader}
     */
    private static function readMessage(array $payload): array
    {
        $reader = new ValueReader($payload);
        if ($reader->readBytes() !== self::NAME) {
            throw new ProtocolException('The message is no worker management message.');
        }

        return [$reader->readU16(), $reader];
    }

    private static function createMessage(int $kind): ValueWriter
    {
        return new ValueWriter([self::NAME, $kind]);
    }
}
