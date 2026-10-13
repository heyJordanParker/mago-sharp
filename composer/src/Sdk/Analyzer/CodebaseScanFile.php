<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\Analyzer\Metadata\ClassLikeMetadata;
use Mago\Sdk\Analyzer\Metadata\FunctionLikeMetadata;
use Mago\Sdk\Analyzer\Metadata\PropertyMetadata;

/**
 * The declarations Mago scanned from one selected source file, before inheritance is resolved.
 *
 * @api
 */
final class CodebaseScanFile
{
    /**
     * @param list<ClassLikeMetadata> $classLikes
     * @param array<string, list<PropertyMetadata>> $properties each class-like's declared properties, by its name
     * @param list<FunctionLikeMetadata> $functionLikes every function, method, closure, and arrow function
     * @internal
     */
    public function __construct(
        public readonly string $path,
        public readonly array $classLikes,
        private readonly array $properties,
        public readonly array $functionLikes,
    ) {}

    /**
     * @return list<PropertyMetadata>
     */
    public function getProperties(ClassLikeMetadata $classLike): array
    {
        return $this->properties[$classLike->name] ?? [];
    }
}
