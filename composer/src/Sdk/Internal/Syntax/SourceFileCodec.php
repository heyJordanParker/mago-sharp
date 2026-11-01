<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Syntax;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Protocol\PayloadReader;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Syntax\SourceFile;

use function count;
use function Mago\Internal\release_mismatch;
use function strlen;

/**
 * Decodes the syntax snapshot shared by external linter and analyzer messages.
 *
 * @internal
 */
final class SourceFileCodec
{
    private const MAXIMUM_NODE_KINDS = 65_536;

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
            throw new ProtocolException(release_mismatch("sent {$count} node kinds", 'knows ' . count($kinds)));
        }

        for ($index = 0; $index < $count; ++$index) {
            $name = $reader->readString();
            if ($name !== $kinds[$index]->value) {
                throw new ProtocolException(release_mismatch(
                    "names node kind {$index} `{$name}`",
                    "names it `{$kinds[$index]->value}`",
                ));
            }
        }

        return $kinds;
    }
}
