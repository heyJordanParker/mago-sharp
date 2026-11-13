<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Type;

use Mago\Sdk\Exception\InvalidArgumentException;

use function strtolower;

/**
 * A function, method or closure.
 *
 * A method carries its class's rule as the host answers it, spec section 24: `$sharp` when the class is a PHP#
 * class-like, found only by its exact name, and `$exactNames` when the class keys its methods by their declared names,
 * as a PHP# class-like and a plain PHP class below a PHP# parent or interface do. An identifier a plugin builds has
 * neither, and is compared by the rule of the identifier it is compared with.
 *
 * @api
 * @mago-expect lint:cyclomatic-complexity
 */
final class FunctionLikeIdentifier
{
    public function __construct(
        public readonly FunctionLikeKind $kind,
        public readonly string $name,
        public readonly ?string $class = null,
        public readonly bool $sharp = false,
        public readonly bool $exactNames = false,
    ) {
        if ($name === '') {
            throw new InvalidArgumentException('A function-like identifier name cannot be empty.');
        }

        if ($kind === FunctionLikeKind::Method ? $class === null || $class === '' : $class !== null) {
            throw new InvalidArgumentException('Only a method identifier requires a non-empty class name.');
        }

        if ($kind !== FunctionLikeKind::Method && ($sharp || $exactNames)) {
            throw new InvalidArgumentException('Only a method identifier carries its class\'s rule.');
        }

        if ($sharp && !$exactNames) {
            throw new InvalidArgumentException('A PHP# class-like keys its methods by their declared names.');
        }
    }

    /**
     * Whether both name one function-like: a function in any case, a closure by its name, and a method by its class's
     * rule, so `Box::total` and `Box::Total` of a class with exact names are two methods.
     */
    public function equals(self $other): bool
    {
        if ($this->kind !== $other->kind) {
            return false;
        }

        if ($this->kind === FunctionLikeKind::Closure) {
            return $this->name === $other->name;
        }

        // Both are of one kind, so both name a class or neither does.
        $name = $this->exactNames || $other->exactNames
            ? $this->name === $other->name
            : strtolower($this->name) === strtolower($other->name);
        $class = $this->sharp || $other->sharp
            ? $this->class === $other->class
            : strtolower($this->class ?? '') === strtolower($other->class ?? '');

        return $name && $class;
    }
}
