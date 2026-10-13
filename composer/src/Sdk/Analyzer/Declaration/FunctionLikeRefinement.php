<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Declaration;

use Mago\Sdk\Exception\InvalidArgumentException;

/**
 * What one function, closure, or arrow function declaration states beyond its native signature.
 *
 * A named function is addressed by its name; a closure or arrow function by the byte offset its
 * declaration starts at, which is how the scanner names it.
 *
 * @api
 */
final class FunctionLikeRefinement
{
    /**
     * @param string $file the path of the scanned source file declaring the function
     * @param string $function the function's fully qualified name, or empty for a closure
     * @param list<RefinementIssue> $issues problems the hook found in the declaration
     * @param int<0, max>|null $declaredAt the byte offset a closure's declaration starts at
     */
    public function __construct(
        public readonly string $file,
        public readonly string $function,
        public readonly SignatureRefinement $signature,
        public readonly array $issues = [],
        public readonly ?int $declaredAt = null,
    ) {
        if ($file === '' || ($function === '') === ($declaredAt === null)) {
            throw new InvalidArgumentException(
                'A function-like refinement must name its file and either its function or where a closure starts.',
            );
        }

        if ($signature->receiver !== null && $declaredAt === null) {
            throw new InvalidArgumentException('Only a closure or arrow function declares the `$this` it runs with.');
        }
    }
}
