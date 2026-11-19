<?php

declare(strict_types=1);

namespace Mago\Sdk;

/**
 * Context provided to a worker reducer in the interpreter that reduces every interpreter's payload.
 *
 * @api
 */
final class WorkerReductionContext
{
    /**
     * @param non-empty-list<string> $workerPayloads Contributions in stable thread order.
     *
     * @internal Constructed by the worker runtime.
     */
    public function __construct(
        public readonly array $workerPayloads,
    ) {}
}
