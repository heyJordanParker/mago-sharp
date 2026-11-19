<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Metadata;

use Mago\Sdk\Analyzer\Type\Visibility;
use Mago\Sdk\SourceLocation;

/**
 * @api
 */
final class PropertyMetadata
{
    /**
     * @param list<AttributeMetadata> $attributes
     * @param array<string, PropertyHookMetadata> $hooks
     * @param list<VersionRange> $availableVersions
     * @param bool $isGenerated whether the compiler added the property, which no source declares, as `$flags` holds
     *   `MetadataFlags::GENERATED`
     */
    public function __construct(
        public readonly string $name,
        public readonly ?SourceLocation $location,
        public readonly ?SourceLocation $nameLocation,
        public readonly Visibility $readVisibility,
        public readonly Visibility $writeVisibility,
        public readonly ?TypeMetadata $declaredType,
        public readonly ?TypeMetadata $type,
        public readonly ?TypeMetadata $writeType,
        public readonly ?TypeMetadata $defaultType,
        public readonly array $attributes,
        public readonly MetadataFlags $flags,
        public readonly array $hooks,
        public readonly array $availableVersions,
        public readonly bool $isGenerated,
    ) {}
}
