<?php

declare(strict_types=1);

namespace Mago\Sdk\Syntax;

use Mago\Sdk\Exception\InvalidArgumentException;

use function count;
use function in_array;
use function str_starts_with;

/**
 * A structured view over a function, method, or static-method call.
 *
 * @api
 * @mago-expect lint:cyclomatic-complexity
 * @mago-expect lint:excessive-parameter-list
 * @mago-expect lint:kan-defect
 */
final class CallExpression
{
    /** @var list<CallArgument> */
    public readonly array $arguments;

    /**
     * @param list<CallArgument> $arguments
     */
    private function __construct(
        public readonly Node $node,
        public readonly Node $callee,
        public readonly ?Node $receiver,
        public readonly ?Node $member,
        array $arguments,
        private readonly bool $static,
        private readonly bool $function,
    ) {
        $this->arguments = $arguments;
    }

    public static function fromNode(SourceFile $source, Node $node): self
    {
        if (
            $node->kind !== NodeKind::FunctionCall
            && $node->kind !== NodeKind::MethodCall
            && $node->kind !== NodeKind::NullSafeMethodCall
            && $node->kind !== NodeKind::StaticMethodCall
        ) {
            throw new InvalidArgumentException('A call-expression view requires a call node.');
        }

        $children = $source->getChildren($node);
        $function = $node->kind === NodeKind::FunctionCall;
        $callee = self::unwrapExpression(
            $source,
            $children[0] ?? throw new InvalidArgumentException('A call node has no callee.'),
        );
        $member = $function ? null : $children[1] ?? null;
        $argumentList = $children[$function ? 1 : 2] ?? null;
        if ($argumentList === null || $argumentList->kind !== NodeKind::ArgumentList) {
            throw new InvalidArgumentException('A call node has no argument list.');
        }

        $arguments = [];
        foreach ($source->getChildren($argumentList) as $argument) {
            $variant = $source->getChildren($argument)[0] ?? null;
            if ($variant === null) {
                continue;
            }
            $parts = $source->getChildren($variant);
            $named = $variant->kind === NodeKind::NamedArgument;
            $value = $parts[$named ? 1 : 0] ?? null;
            if ($value === null) {
                continue;
            }
            $value = self::unwrapExpression($source, $value);
            $arguments[] = new CallArgument(
                count($arguments),
                $argument,
                $value,
                $named ? $source->getText($parts[0]) : null,
                str_starts_with($source->getText($variant), '...'),
            );
        }

        // Mago names a class at the member of `PaymentResult.Declined(…)`, which builds that enum case, spec section 20,
        // as `new PaymentResult\Declined(…)` does: no call of a function or a method.
        $memberName = $member === null ? null : $source->getResolvedName($member);
        $builds = $memberName?->binding === Binding::ClassName;
        // A PHP# static call is a method call whose receiver is a bare name Mago bound to a class, `Calc.make()`, the
        // enclosing class, `Self.cap()`, as PHP's `static::cap()`, or its parent, `super.save()` or
        // `super<HasLabel>.label()`, as PHP's `parent::save()`.
        $static =
            $node->kind === NodeKind::StaticMethodCall
            || $node->kind === NodeKind::MethodCall
            && !$builds
            && (
                $callee->kind === NodeKind::ConstantAccess
                && $source->getResolvedName($callee)?->binding === Binding::ClassName
                || $callee->kind === NodeKind::Keyword
                && in_array($source->getText($callee), ['Self', 'super'], true)
                || $callee->kind === NodeKind::SuperOf
            );
        // A PHP# call of a plain PHP function, `global.max()` or `Pdf.render()`, is a method call whose member Mago
        // names as the function.
        if ($memberName !== null && !$builds) {
            return new self($node, $member, null, $member, $arguments, false, true);
        }

        return new self($node, $callee, $function || $builds ? null : $callee, $member, $arguments, $static, $function);
    }

    public static function fromExpression(SourceFile $source, Node $node): ?self
    {
        $node = self::unwrapExpression($source, $node);
        while ($node->kind === NodeKind::Call) {
            $next = $source->getChildren($node)[0] ?? null;
            if ($next === null) {
                break;
            }
            $node = self::unwrapExpression($source, $next);
        }

        return match ($node->kind) {
            NodeKind::FunctionCall,
            NodeKind::MethodCall,
            NodeKind::NullSafeMethodCall,
            NodeKind::StaticMethodCall,
                => self::fromNode($source, $node),
            default => null,
        };
    }

    public function isFunction(): bool
    {
        return $this->function;
    }

    public function isStaticMethod(): bool
    {
        return $this->static;
    }

    public function isMethod(): bool
    {
        return (
            ($this->node->kind === NodeKind::MethodCall || $this->node->kind === NodeKind::NullSafeMethodCall)
            && !$this->static
            && !$this->function
            && $this->receiver !== null
        );
    }

    public function getName(SourceFile $source): ?string
    {
        if ($this->member === null) {
            $callee = $this->callee;
            while ($callee->kind === NodeKind::ConstantAccess || $callee->kind === NodeKind::Expression) {
                $next = $source->getChildren($callee)[0] ?? null;
                if ($next === null) {
                    break;
                }
                $callee = $next;
            }

            // A PHP# call of a local, `twice(1)`, calls the function value it holds, which has no name, as `$twice(1)`
            // has none: Mago names no function there.
            return match ($callee->kind) {
                NodeKind::Identifier,
                NodeKind::LocalIdentifier,
                NodeKind::QualifiedIdentifier,
                NodeKind::FullyQualifiedIdentifier,
                    => $source->getResolvedName($callee) === null ? null : $source->getText($callee),
                default => null,
            };
        }

        $selector = $source->getChildren($this->member)[0] ?? null;

        if ($selector === null || $selector->kind !== NodeKind::LocalIdentifier) {
            return null;
        }

        return $source->getText($selector);
    }

    private static function unwrapExpression(SourceFile $source, Node $node): Node
    {
        while ($node->kind === NodeKind::Expression) {
            $next = $source->getChildren($node)[0] ?? null;
            if ($next === null) {
                break;
            }
            $node = $next;
        }

        return $node;
    }
}
