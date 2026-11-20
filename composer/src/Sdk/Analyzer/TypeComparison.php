<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use function serialize;

/**
 * One native type comparison, suitable for batched evaluation.
 *
 * @api
 */
final class TypeComparison
{
    /** @var null|non-empty-list<null|bool|int|float|string|list<mixed>> */
    private ?array $leftEncoding = null;

    /** @var null|non-empty-list<null|bool|int|float|string|list<mixed>> */
    private ?array $rightEncoding = null;

    private ?string $cacheKey = null;

    public function __construct(
        public readonly TypeComparisonKind $kind,
        public readonly Type $left,
        public readonly Type $right,
    ) {}

    public static function equal(Type $left, Type $right): self
    {
        return new self(TypeComparisonKind::Equal, $left, $right);
    }

    public static function containedBy(Type $input, Type $container): self
    {
        return new self(TypeComparisonKind::ContainedBy, $input, $container);
    }

    public static function canBeIdentical(Type $left, Type $right): self
    {
        return new self(TypeComparisonKind::CanBeIdentical, $left, $right);
    }

    /**
     * @return non-empty-list<null|bool|int|float|string|list<mixed>>
     * @internal
     */
    public function encodeLeft(): array
    {
        return $this->leftEncoding ??= $this->left->encode();
    }

    /**
     * @return non-empty-list<null|bool|int|float|string|list<mixed>>
     * @internal
     */
    public function encodeRight(): array
    {
        return $this->rightEncoding ??= $this->right->encode();
    }

    /** @internal */
    public function cacheKey(): string
    {
        return $this->cacheKey ??= serialize([$this->kind->value, $this->encodeLeft(), $this->encodeRight()]);
    }
}
