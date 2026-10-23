<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Syntax;

use Composer\InstalledVersions;
use Mago\Sdk\Exception\ProtocolException;
use Mago\Sdk\Internal\Protocol\PayloadReader;
use Mago\Sdk\Internal\Protocol\PayloadWriter;
use Mago\Sdk\Internal\Syntax\SourceFileCodec;
use Mago\Sdk\Syntax\NodeKind;
use PHPUnit\Framework\TestCase;

use function array_slice;
use function count;

final class SourceFileCodecTest extends TestCase
{
    public function testAMagoWithAnotherNumberOfNodeKindsNamesBothCountsAndTheFix(): void
    {
        $kinds = NodeKind::cases();
        $sent = array_slice($kinds, 0, count($kinds) - 1);

        $this->expectExceptionObject(new ProtocolException(
            '`mago` sent '
            . count($sent)
            . ' node kinds, and the extension SDK, heyjordanparker/mago-sharp '
            . self::sdkVersion()
            . ', knows '
            . count($kinds)
            . '. `mago` and heyjordanparker/mago-sharp must be the '
            . 'same version: run the `mago` that heyjordanparker/mago-sharp installs, or require the '
            . 'heyjordanparker/mago-sharp version of your `mago`.',
        ));

        SourceFileCodec::readNodeKinds(self::table($sent));
    }

    public function testAMagoThatNamesANodeKindOtherwiseNamesThePlaceAndBothNames(): void
    {
        $kinds = NodeKind::cases();
        $sent = [...array_slice($kinds, 0, 2), 'Renamed', ...array_slice($kinds, 3)];

        $this->expectExceptionObject(new ProtocolException(
            '`mago` names node kind 2 `Renamed`, and the extension SDK, heyjordanparker/mago-sharp '
            . self::sdkVersion()
            . ', names it `'
            . $kinds[2]->value
            . '`. `mago` and heyjordanparker/mago-sharp must '
            . 'be the same version: run the `mago` that heyjordanparker/mago-sharp installs, or require the '
            . 'heyjordanparker/mago-sharp version of your `mago`.',
        ));

        SourceFileCodec::readNodeKinds(self::table($sent));
    }

    public function testTheSameTableReadsAsTheSdkNodeKinds(): void
    {
        $kinds = NodeKind::cases();
        $reader = self::table($kinds);

        self::assertSame($kinds, SourceFileCodec::readNodeKinds($reader));
        $reader->finish();
    }

    /**
     * The node-kind table `mago` sends in a describe request, holding `kinds` in order.
     *
     * @param list<NodeKind|string> $kinds
     */
    private static function table(array $kinds): PayloadReader
    {
        $writer = new PayloadWriter();
        $writer->writeCount($kinds);
        foreach ($kinds as $kind) {
            $writer->writeString($kind instanceof NodeKind ? $kind->value : $kind);
        }

        return new PayloadReader($writer->finish());
    }

    private static function sdkVersion(): string
    {
        return (string) InstalledVersions::getPrettyVersion('heyjordanparker/mago-sharp');
    }
}
