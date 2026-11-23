<?php

declare(strict_types=1);

namespace Mago\Sdk;

use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Internal\Analyzer\ReportedIssue;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;

/**
 * Context provided to a worker reducer in the interpreter that reduces every interpreter's payload.
 *
 * @api
 */
final class WorkerReductionContext
{
    /**
     * @var list<ReportedIssue>
     */
    private array $issues = [];

    /**
     * @param non-empty-list<string> $workerPayloads Contributions in stable thread order.
     *
     * @internal Constructed by the worker runtime.
     */
    public function __construct(
        public readonly array $workerPayloads,
    ) {}

    /**
     * Reports `$issue` in the analysis the payloads come from, as an after-analysis hook reports one. Its code is
     * `$code` under the extension's identifier, and its primary annotation names the file it points into.
     */
    public function report(Level $level, string $code, Issue $issue): void
    {
        if ($code === '') {
            throw new InvalidArgumentException('A worker reducer issue code cannot be empty.');
        }

        $this->issues[] = new ReportedIssue($level, $code, $issue);
    }

    /**
     * @internal
     *
     * @return list<ReportedIssue>
     */
    public function takeReportedIssues(): array
    {
        $issues = $this->issues;
        $this->issues = [];

        return $issues;
    }
}
