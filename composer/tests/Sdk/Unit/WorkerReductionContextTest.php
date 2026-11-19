<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Extension;
use Mago\Sdk\WorkerReducer;
use Mago\Sdk\WorkerReductionContext;
use PHPUnit\Framework\TestCase;

final class WorkerReductionContextTest extends TestCase
{
    public function testPreservesOrderedPayloads(): void
    {
        $context = new WorkerReductionContext(['first', '', 'third']);

        self::assertSame(['first', '', 'third'], $context->workerPayloads);
    }

    public function testExtensionRetainsItsWorkerReducer(): void
    {
        $reducer = new class() implements WorkerReducer {
            public function collect(): string
            {
                return '';
            }

            public function reduce(WorkerReductionContext $context): void {}
        };

        $extension = new Extension('acme/reducer', 'Reducer', '1.0.0', workerReducer: $reducer);

        self::assertSame($reducer, $extension->workerReducer);
    }
}
