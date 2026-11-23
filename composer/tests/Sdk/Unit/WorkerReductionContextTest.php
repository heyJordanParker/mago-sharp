<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
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

    /**
     * A reducer reports each issue its combined result shows, with a level and a code as an after-analysis hook does,
     * and the worker takes them once.
     */
    public function testAReducerReportsIssuesTheWorkerTakesOnce(): void
    {
        $context = new WorkerReductionContext(['first']);
        $issue = Issue::at('This body repeats another.', new SourceLocation('src/Second.php', new Span(3, 9)));
        $context->report(Level::Error, 'duplicate-body', $issue);
        $context->report(Level::Warning, 'near-duplicate', $issue);

        $reported = $context->takeReportedIssues();

        self::assertSame(
            [[Level::Error, 'duplicate-body', $issue], [Level::Warning, 'near-duplicate', $issue]],
            array_map(static fn($reported): array => [$reported->level, $reported->code, $reported->issue], $reported),
        );
        self::assertSame([], $context->takeReportedIssues());
    }

    public function testAReducerIssueNeedsACode(): void
    {
        $this->expectException(InvalidArgumentException::class);
        $this->expectExceptionMessage('A worker reducer issue code cannot be empty.');

        (new WorkerReductionContext(['first']))->report(
            Level::Error,
            '',
            Issue::at('This body repeats another.', new SourceLocation('src/Second.php', new Span(3, 9))),
        );
    }
}
