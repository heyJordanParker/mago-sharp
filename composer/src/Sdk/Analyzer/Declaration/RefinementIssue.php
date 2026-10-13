<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Span;

/**
 * A problem a codebase-scan hook found while reading a declaration.
 *
 * Mago reports it under the hook's plugin, at the span in the refined declaration's file.
 *
 * @api
 */
final class RefinementIssue
{
    public function __construct(
        public readonly Level $level,
        public readonly string $code,
        public readonly string $message,
        public readonly Span $span,
    ) {
        if ($code === '' || $message === '') {
            throw new InvalidArgumentException('A refinement issue must have a code and a message.');
        }
    }
}
