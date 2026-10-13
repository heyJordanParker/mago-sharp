<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Span;

/**
 * A refined type and the source range that states it, where diagnostics about it are reported.
 *
 * @api
 */
final class RefinedType
{
    public function __construct(
        public readonly Type $type,
        public readonly Span $span,
    ) {}
}
