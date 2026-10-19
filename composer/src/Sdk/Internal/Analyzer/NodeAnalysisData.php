<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Analyzer\VariableDefinedness;
use Mago\Sdk\Syntax\NodeKind;

/**
 * @internal
 * @mago-expect lint:excessive-parameter-list
 */
final class NodeAnalysisData
{
    /**
     * @param list<Type|null> $argumentTypes
     * @param array<string, VariableDefinedness>|null $variableDefinedness
     * @param list<int<0, 65535>> $targetedHookIndices
     */
    public function __construct(
        public readonly ?Type $targetType,
        public readonly ?Type $receiverType,
        public readonly array $argumentTypes,
        public readonly ?array $variableDefinedness,
        public readonly array $targetedHookIndices,
        /**
         * The kind of node a PHP# method call means when it is not a method call: `StaticMethodCall` for
         * `Calc.make()`, `FunctionCall` for `global.max()` or `Pdf.render()` after `import Vendor.Pdf;`, and
         * `Instantiation` for `PaymentResult.Declined(…)`, which builds an enum case.
         */
        public readonly ?NodeKind $meaning,
    ) {}
}
