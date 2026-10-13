<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Analyzer\VariableDefinedness;

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
        public readonly bool $isSharpStaticCall,
    ) {}
}
