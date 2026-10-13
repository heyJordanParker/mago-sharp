<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\CancellationTokenInterface;
use Mago\Sdk\PHPVersion;

/**
 * The magic access Mago asks a forwarding provider about.
 *
 * `class` is the class the member was requested on, `member` the requested
 * method or property name, and `receiverType` the object it was requested on.
 *
 * @api
 * @mago-expect lint:excessive-parameter-list
 */
final class CallForwardingProviderContext
{
    /**
     * @param non-empty-string $class
     * @param non-empty-string $member
     */
    public function __construct(
        public readonly PHPVersion $phpVersion,
        public readonly Codebase $codebase,
        public readonly string $class,
        public readonly string $member,
        public readonly bool $property,
        public readonly Type $receiverType,
        public readonly TypeComparator $types,
        public readonly CancellationTokenInterface $cancellation,
    ) {}
}
