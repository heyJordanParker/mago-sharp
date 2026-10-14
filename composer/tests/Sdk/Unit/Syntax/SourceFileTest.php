<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Syntax;

use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Analyzer\Protocol as AnalyzerProtocol;
use Mago\Sdk\Internal\Linter\Protocol as LinterProtocol;
use Mago\Sdk\Internal\Syntax\NodeStore;
use Mago\Sdk\Internal\Syntax\ResolvedNameStore;
use Mago\Sdk\Internal\Syntax\TriviaStore;
use Mago\Sdk\PHPVersion;
use Mago\Sdk\Syntax\Binding;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Syntax\SourceFile;
use Mago\Sdk\Syntax\TriviaKind;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

use function array_fill;
use function pack;
use function strlen;

final class SourceFileTest extends TestCase
{
    public function testPackedSourceDataIsExposed(): void
    {
        $noNode = 4_294_967_295;
        $nodeRecords =
            pack('nNNNNN', 0, 0, 10, $noNode, 1, $noNode)
            . pack('nNNNNN', 1, 1, 4, 0, $noNode, 2)
            . pack('nNNNNN', 1, 5, 8, 0, $noNode, $noNode);
        $nodeStore = new NodeStore([NodeKind::Program, NodeKind::FunctionCall], $nodeRecords, 3);
        $resolvedName = 'Psl\\Iter\\any';
        $nameStarts = pack('N', 1);
        $nameRecords = pack('NNNCC', 4, 0, strlen($resolvedName), 0, 1);
        $nameStore = new ResolvedNameStore($nameStarts, $nameRecords, $resolvedName, 1);
        $triviaStore = new TriviaStore(pack('CNN', 4, 0, 10), 1);
        $sourceFile = new SourceFile(
            PHPVersion::fromParts(8, 3),
            'fixture.php',
            '0123456789',
            [1, 2],
            $nodeStore,
            $nameStore,
            $triviaStore,
        );

        $targets = $sourceFile->getTargetNodes();
        self::assertCount(2, $targets);
        self::assertSame(NodeKind::FunctionCall, $targets[0]->kind);
        self::assertCount(3, $sourceFile->getNodes());
        self::assertSame($targets, $sourceFile->getNodes(NodeKind::FunctionCall));
        self::assertSame($targets, $sourceFile->getChildren($sourceFile->getNode(0)));
        self::assertSame(1, $sourceFile->getFirstDescendant($sourceFile->getNode(0), NodeKind::FunctionCall)?->id);
        self::assertNull($sourceFile->getFirstDescendant($sourceFile->getNode(0), NodeKind::LiteralString));
        self::assertSame(0, $sourceFile->getParent($targets[0])?->id);
        self::assertSame('123', $sourceFile->getText($targets[0]));
        self::assertSame($resolvedName, $sourceFile->getResolvedName($targets[0])?->name);
        self::assertSame(Binding::ClassName, $sourceFile->getResolvedName($targets[0])?->binding);
        self::assertSame(TriviaKind::DocBlockComment, $sourceFile->getTrivia()[0]->kind);
    }

    public function testANodeKindAboveTwoHundredFiftyFiveIsDecoded(): void
    {
        $noNode = 4_294_967_295;
        $kinds = array_fill(0, 300, NodeKind::Program);
        $kinds[299] = NodeKind::FunctionCall;
        $nodeRecords = pack('nNNNNN', 0, 0, 10, $noNode, 1, $noNode) . pack('nNNNNN', 299, 1, 4, 0, $noNode, $noNode);

        $scanned = new NodeStore($kinds, $nodeRecords, 2);
        $call = $scanned->getAll(NodeKind::FunctionCall)[0];
        self::assertSame(NodeKind::FunctionCall, $call->kind);
        self::assertSame(1, $call->span->start);
        self::assertSame(4, $call->span->end);
        self::assertSame(0, $call->parentId);
        self::assertSame([$call], $scanned->getChildren($scanned->get(0)));

        $looked = new NodeStore($kinds, $nodeRecords, 2);
        self::assertSame(NodeKind::FunctionCall, $looked->getMany([1])[0]->kind);
    }

    /**
     * @return iterable<string, array{class-string<AnalyzerProtocol|LinterProtocol>, string, string}>
     */
    public static function previousProtocolVersions(): iterable
    {
        yield 'analyzer' => [
            AnalyzerProtocol::class,
            pack('N3', 0x4D41_4E41, 0x0001_000A, 1 << 16),
            'Unsupported analyzer protocol version 1.10.',
        ];
        yield 'linter' => [
            LinterProtocol::class,
            pack('N3', 0x4D4C_4E54, 0x0001_0001, 1 << 16),
            'Unsupported linter protocol version 1.1.',
        ];
    }

    /**
     * @param class-string<AnalyzerProtocol|LinterProtocol> $protocol
     */
    #[DataProvider('previousProtocolVersions')]
    public function testASnapshotMessageFromThePreviousProtocolVersionIsRefused(
        string $protocol,
        string $header,
        string $message,
    ): void {
        $this->expectException(ProtocolException::class);
        $this->expectExceptionMessage($message);

        $protocol::readRequest($header);
    }

    public function testABindingIsOneTheSnapshotCanCarry(): void
    {
        self::assertSame([Binding::ClassName, Binding::Constant, Binding::Member], Binding::cases());
    }
}
