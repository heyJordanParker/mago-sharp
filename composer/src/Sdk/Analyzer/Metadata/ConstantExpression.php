<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Metadata;

use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\SourceLocation;

use function array_key_exists;
use function constant;

/**
 * An attribute argument as PHP evaluates it, with every class name resolved.
 *
 * Constants stay references until evaluated, because PHP reads their values when the
 * attribute is instantiated.
 *
 * @api
 * @mago-expect lint:excessive-parameter-list
 */
final class ConstantExpression
{
    /**
     * @param list<array{ConstantExpression|null, ConstantExpression}> $items an array's keys and values
     * @param list<array{string|null, ConstantExpression}> $arguments a constructor's argument names and values
     */
    public function __construct(
        public readonly ConstantExpressionKind $kind,
        public readonly bool|int|float|string|null $value = null,
        public readonly ?string $constant = null,
        public readonly array $items = [],
        public readonly array $arguments = [],
        public readonly ?SourceLocation $location = null,
    ) {}

    /**
     * @mago-expect analysis:unknown-class-instantiation A `new` expression names its class only in the evaluated source.
     */
    public function evaluate(): mixed
    {
        return match ($this->kind) {
            ConstantExpressionKind::Literal, ConstantExpressionKind::ClassName => $this->value,
            ConstantExpressionKind::ClassConstant => constant("{$this->value}::{$this->constant}"),
            ConstantExpressionKind::Constant => constant((string) $this->value),
            ConstantExpressionKind::Array_ => $this->evaluateArray(),
            ConstantExpressionKind::New_ => new ((string) $this->value)(...self::evaluateArguments($this->arguments)),
            ConstantExpressionKind::Unsupported => throw new InvalidArgumentException(
                "The expression at {$this->location?->span->start} in {$this->location?->file} is not a constant expression Mago evaluates.",
            ),
        };
    }

    /**
     * @param list<array{string|null, ConstantExpression}> $arguments
     *
     * @return array<int|string, mixed>
     */
    public static function evaluateArguments(array $arguments): array
    {
        $evaluated = [];
        foreach ($arguments as [$name, $argument]) {
            if ($name === null) {
                $evaluated[] = $argument->evaluate();

                continue;
            }

            if (array_key_exists($name, $evaluated)) {
                throw new InvalidArgumentException("The argument `{$name}` is named twice.");
            }

            $evaluated[$name] = $argument->evaluate();
        }

        return $evaluated;
    }

    /**
     * @return array<array-key, mixed>
     */
    private function evaluateArray(): array
    {
        $array = [];
        foreach ($this->items as [$key, $item]) {
            if ($key === null) {
                $array[] = $item->evaluate();

                continue;
            }

            $array[$key->evaluate()] = $item->evaluate();
        }

        return $array;
    }
}
