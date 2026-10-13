<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

/**
 * What one method, function, or closure declaration states beyond its native signature.
 *
 * @api
 * @mago-expect lint:excessive-parameter-list
 */
final class SignatureRefinement
{
    /**
     * @param list<TypeParameter> $typeParameters the declaration's own type parameters, in order
     * @param array<non-empty-string, RefinedType> $parameters keyed by parameter name without `$`
     * @param array<non-empty-string, RefinedType> $closureThis the object a closure passed to each
     *        parameter runs with as `$this`, keyed by parameter name without `$`
     * @param RefinedType|null $receiver the object a closure or arrow function runs with as `$this`
     * @param bool $returnFromBody whether a method's return is taken from what its body returns once
     *        the codebase is populated, keeping its native return when that cannot be resolved
     */
    public function __construct(
        public readonly array $typeParameters = [],
        public readonly array $parameters = [],
        public readonly ?RefinedType $return = null,
        public readonly array $closureThis = [],
        public readonly ?RefinedType $receiver = null,
        public readonly bool $returnFromBody = false,
    ) {}
}
