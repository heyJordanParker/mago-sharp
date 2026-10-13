<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Metadata;

use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\SourceLocation;

use function in_array;

/**
 * @api
 * @mago-expect lint:cyclomatic-complexity
 */
final class AttributeMetadata
{
    /**
     * @param list<AttributeArgumentMetadata> $arguments
     */
    public function __construct(
        public readonly string $name,
        public readonly SourceLocation $location,
        public readonly array $arguments,
    ) {}

    public function getArgument(int $position, string ...$names): ?AttributeArgumentMetadata
    {
        foreach ($this->arguments as $argument) {
            if ($argument->name !== null && in_array($argument->name, $names, true)) {
                return $argument;
            }
        }

        $index = 0;
        foreach ($this->arguments as $argument) {
            if ($argument->name !== null) {
                continue;
            }
            if ($index++ === $position) {
                return $argument;
            }
        }

        return null;
    }

    /**
     * Evaluates the arguments as PHP's `ReflectionAttribute::getArguments()` does.
     *
     * @return array<int|string, mixed>
     */
    public function getArguments(): array
    {
        $arguments = [];
        foreach ($this->arguments as $argument) {
            if ($argument->value === null) {
                throw new InvalidArgumentException("An argument of `{$this->name}` is a placeholder.");
            }

            $arguments[] = [$argument->name, $argument->value];
        }

        return ConstantExpression::evaluateArguments($arguments);
    }

    /**
     * Instantiates the attribute as PHP's `ReflectionAttribute::newInstance()` does.
     *
     * @mago-expect analysis:unknown-class-instantiation An attribute names its class only in the scanned source.
     */
    public function newInstance(): object
    {
        return new $this->name(...$this->getArguments());
    }
}
