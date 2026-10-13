<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Exception\InvalidArgumentException;

/**
 * What one class-like declaration states beyond its native syntax.
 *
 * A codebase-scan hook returns this for a class in a file it received. Mago installs it before the
 * codebase is populated, so inheritance, method bodies, and call sites all see the refined types,
 * while the native declarations stay in place and an incompatible refinement is still reported.
 *
 * @api
 * @mago-expect lint:excessive-parameter-list
 */
final class ClassLikeRefinement
{
    /**
     * @param string $file the path of the scanned source file declaring the class
     * @param string $class the class's fully qualified name, or empty for an anonymous class
     * @param list<TypeParameter> $typeParameters the class's own type parameters, in order
     * @param list<InheritedArguments> $inherited arguments applied to direct parents
     * @param array<non-empty-string, RefinedType> $properties keyed by property name without `$`
     * @param array<non-empty-string, SignatureRefinement> $methods keyed by method name
     * @param list<RefinementIssue> $issues problems the hook found in the declaration
     * @param int<0, max>|null $declaredAt the byte offset an anonymous class's declaration starts at
     * @param list<non-empty-string> $requiredExtends the classes every user of a trait must extend
     */
    public function __construct(
        public readonly string $file,
        public readonly string $class,
        public readonly array $typeParameters = [],
        public readonly array $inherited = [],
        public readonly array $properties = [],
        public readonly array $methods = [],
        public readonly array $issues = [],
        public readonly ?int $declaredAt = null,
        public readonly array $requiredExtends = [],
    ) {
        if ($file === '' || ($class === '') === ($declaredAt === null)) {
            throw new InvalidArgumentException(
                'A class-like refinement must name its file and either its class or where an anonymous class starts.',
            );
        }
    }
}
