<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\Span;

/**
 * The arguments a declaration applies to a generic direct parent class or interface.
 *
 * @api
 */
final class InheritedArguments
{
    /**
     * @param string $ancestor the parent's fully qualified name
     * @param list<Type> $arguments in the parent's type-parameter order
     */
    public function __construct(
        public readonly string $ancestor,
        public readonly array $arguments,
        public readonly Span $span,
    ) {
        if ($ancestor === '') {
            throw new InvalidArgumentException('Inherited arguments must name their parent.');
        }
    }
}
