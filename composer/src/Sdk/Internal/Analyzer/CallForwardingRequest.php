<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Analyzer\Type;

/**
 * @internal
 * @mago-expect lint:excessive-parameter-list
 */
final class CallForwardingRequest
{
    /**
     * @param list<int<0, 65535>> $providerIndices
     * @param non-empty-string $class
     * @param non-empty-string $member
     */
    public function __construct(
        public readonly int $generation,
        public readonly array $providerIndices,
        public readonly string $class,
        public readonly string $member,
        public readonly bool $property,
        public readonly Type $receiverType,
    ) {}
}
