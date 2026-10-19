<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use Mago\Sdk\Analyzer\Metadata\MetadataFlags;
use PHPUnit\Framework\TestCase;
use ReflectionClass;

use function dirname;
use function file_get_contents;
use function preg_match_all;

final class MetadataFlagsTest extends TestCase
{
    /** A host reads every flag the analyzer sets by its name, as the codex's `MetadataFlags` names it. */
    public function testItNamesEveryFlagTheCodexDefines(): void
    {
        $source = file_get_contents(dirname(__DIR__, 5) . '/crates/codex/src/metadata/flags.rs');
        self::assertIsString($source);
        $matches = [];
        preg_match_all('/pub const ([A-Z_]+): MetadataFlags = MetadataFlags\(1 << (\d+)\);/', $source, $matches);
        $codex = [];
        foreach ($matches[1] as $index => $name) {
            $codex[$name] = 1 << (int) $matches[2][$index];
        }

        self::assertCount(59, $codex);
        self::assertSame($codex, (new ReflectionClass(MetadataFlags::class))->getConstants());
    }
}
