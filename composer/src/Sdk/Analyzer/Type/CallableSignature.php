<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Type;

use Mago\Sdk\Analyzer\Type;

/**
 * @api
 * @mago-expect lint:excessive-parameter-list
 */
final class CallableSignature
{
    /**
     * @param list<CallableParameter> $parameters
     * @param list<CallableConstraint> $constraints
     */
    public function __construct(
        public readonly bool $pure,
        public readonly bool $closure,
        public readonly array $parameters,
        public readonly ?Type $returnType,
        /**
         * The function-like the signature comes from. A method's names the class the value is read through, as
         * `Child::method(...)` and PHP#'s `child.method` write it, and that class may inherit the method. So
         * `Codebase::getFunctionLike()` returns null for an inherited method, and
         * `Codebase::getDeclaringMethod($source->class, $source->name)` finds the method where it is declared.
         */
        public readonly ?FunctionLikeIdentifier $source,
        public readonly array $constraints,
    ) {}
}
