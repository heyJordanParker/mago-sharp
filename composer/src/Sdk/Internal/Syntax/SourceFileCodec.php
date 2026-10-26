<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Syntax;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Protocol\PayloadReader;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Syntax\SourceFile;
use RuntimeException;

use function count;
use function Mago\Internal\get_version;
use function strlen;

/**
 * Decodes the syntax snapshot shared by external linter and analyzer messages.
 *
 * @internal
 */
final class SourceFileCodec
{
    private const MAXIMUM_NODE_KINDS = 65_536;

    private const PACKAGE = 'heyjordanparker/mago-sharp';

    /**
     * @param list<NodeKind> $kinds
     */
    public static function read(
        PayloadReader $reader,
        PHPVersion $phpVersion,
        array $kinds,
        string $path,
        string $contents,
    ): SourceFile {
        $targetCount = $reader->readU32();
        $targetIds = $reader->readU32List($targetCount);
        $nodeCount = $reader->readU32();
        $nodeBytes = $nodeCount * NodeStore::RECORD_SIZE;
        $nodeRecords = $reader->readRaw($nodeBytes);
        $read = strlen($nodeRecords);
        // `NodeStore` reads each node's kind byte by byte, and a byte past the end of the records reads as 0 with only
        // a warning.
        if ($read !== $nodeBytes) {
            throw new ProtocolException(
                "The source snapshot holds {$read} bytes of node records, where its {$nodeCount} nodes take {$nodeBytes}.",
            );
        }
        $nodes = new NodeStore($kinds, $nodeRecords, $nodeCount);
        $nameCount = $reader->readU32();
        $nameStarts = $reader->readRaw($nameCount * ResolvedNameStore::START_SIZE);
        $nameRecords = $reader->readRaw($nameCount * ResolvedNameStore::RECORD_SIZE);
        $names = new ResolvedNameStore($nameStarts, $nameRecords, $reader->readBytes(), $nameCount);
        $triviaCount = $reader->readU32();
        $triviaRecords = $reader->readRaw($triviaCount * TriviaStore::RECORD_SIZE);

        return new SourceFile(
            $phpVersion,
            $path,
            $contents,
            $targetIds,
            $nodes,
            $names,
            new TriviaStore($triviaRecords, $triviaCount),
        );
    }

    /**
     * @return list<NodeKind>
     */
    public static function readNodeKinds(PayloadReader $reader): array
    {
        $kinds = NodeKind::cases();
        $count = $reader->readCount(self::MAXIMUM_NODE_KINDS);
        if ($count !== count($kinds)) {
            throw self::differentNodeKinds("sent {$count} node kinds", 'knows ' . count($kinds));
        }

        for ($index = 0; $index < $count; ++$index) {
            $name = $reader->readString();
            if ($name !== $kinds[$index]->value) {
                throw self::differentNodeKinds(
                    "names node kind {$index} `{$name}`",
                    "names it `{$kinds[$index]->value}`",
                );
            }
        }

        return $kinds;
    }

    /**
     * The error for a `mago` whose node-kind table is not this SDK's: what each side holds, this SDK's version when
     * Composer gives one, and the fix, since `mago` and the SDK are released together at one version.
     */
    private static function differentNodeKinds(string $mago, string $sdk): ProtocolException
    {
        $package = self::PACKAGE;
        try {
            $named = "{$package} " . get_version();
        } catch (RuntimeException) {
            $named = $package;
        }

        return new ProtocolException(
            "`mago` {$mago}, and the extension SDK, {$named}, {$sdk}. `mago` and {$package} must be the "
            . "same version: run the `mago` that {$package} installs, or require the {$package} version of your `mago`.",
        );
    }
}
