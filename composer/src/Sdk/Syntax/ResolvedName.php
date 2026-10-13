<?php

declare(strict_types=1);

namespace Mago\Sdk\Syntax;

use Mago\Sdk\Span;

/**
 * A name resolved by Mago within a syntax node.
 *
 * @api
 */
final class ResolvedName
{
    public function __construct(
        public readonly Span $span,
        public readonly string $name,
        public readonly bool $imported,
        /**
         * What the bare PHP# name refers to, or null for a PHP name. Locals and `this` have no resolved name, so a
         * local or `this` never appears here.
         */
        public readonly ?Binding $binding = null,
    ) {}
}
