<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Analyzer\Type\Variance;
use Mago\Sdk\Exception\InvalidArgumentException;

/**
 * One type parameter a class or method declares, with the bound every argument must satisfy
 * and, for a class, how its members use the parameter. A method's own parameter is invariant.
 *
 * @api
 */
final class TypeParameter
{
    public function __construct(
        public readonly string $name,
        public readonly RefinedType $bound,
        public readonly Variance $variance = Variance::Invariant,
    ) {
        if ($name === '') {
            throw new InvalidArgumentException('A type parameter must have a name.');
        }

        if ($variance === Variance::Bivariant) {
            throw new InvalidArgumentException('A type parameter is invariant, covariant, or contravariant.');
        }
    }
}
