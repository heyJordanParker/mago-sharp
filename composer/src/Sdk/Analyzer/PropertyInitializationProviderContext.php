<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\Analyzer\Metadata\PropertyMetadata;
use Mago\Sdk\PHPVersion;

/**
 * Context for deciding whether framework behavior initializes a declared property.
 *
 * The property metadata name includes its leading `$`; provider target names do not.
 *
 * @api
 */
final class PropertyInitializationProviderContext
{
    /** @param non-empty-string $declaringClass */
    public function __construct(
        public readonly PHPVersion $phpVersion,
        public readonly Codebase $codebase,
        public readonly string $declaringClass,
        public readonly PropertyMetadata $property,
        public readonly TypeComparator $types,
    ) {}
}
