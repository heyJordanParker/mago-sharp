<?php

declare(strict_types=1);

namespace Mago\Sdk;

/**
 * Combines what every worker gathered in one analysis, and reports from the combined result.
 *
 * An analysis runs each extension's reducer once, after every worker ran its hooks on every file and before mago
 * writes the report: each worker's interpreter collects its contribution, and one interpreter reduces them all. So a
 * rule that needs the whole codebase gathers per file in its hooks, on whichever worker analyzes the file, and reports
 * from `reduce()`, with no second walk of the files on one thread.
 *
 * The same reducer instance may be shared with linter rules and analyzer plugins so they can accumulate data without
 * communicating between workers.
 *
 * @api
 */
interface WorkerReducer
{
    /**
     * Export what this worker gathered since its last collection as an opaque byte string. A worker keeps its state
     * between the analyses of one run, so a reducer clears what it exported here.
     */
    public function collect(): string;

    /**
     * Reduce every worker contribution on one interpreter, and report each issue the combined result shows through
     * `$context`.
     */
    public function reduce(WorkerReductionContext $context): void;
}
