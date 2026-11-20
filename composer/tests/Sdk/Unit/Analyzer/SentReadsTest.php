<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use Mago\Sdk\Internal\Analyzer\CodebaseReads;
use Mago\Sdk\Internal\Analyzer\SentReads;
use Mago\Sdk\Internal\Protocol\ValueWriter;
use PHPUnit\Framework\TestCase;

final class SentReadsTest extends TestCase
{
    /**
     * A list of reads goes whole the first time a generation sends it and by its number after. Another list gets
     * another number, and a new generation sends a list whole again under a number no earlier list had.
     */
    public function testAListOfReadsGoesWholeOnceAGenerationAndByItsNumberAfter(): void
    {
        $sent = new SentReads();
        $order = self::reads('App\Order');

        self::assertSame([0, true, 1, 1, 0, 'App\Order'], self::written($sent, 7, $order));
        self::assertSame([0, false], self::written($sent, 7, self::reads('App\Order')));
        self::assertSame([1, true, 1, 1, 0, 'App\Line'], self::written($sent, 7, self::reads('App\Line')));
        self::assertSame([2, true, 1, 1, 0, 'App\Order'], self::written($sent, 8, $order));
        self::assertSame([2, false], self::written($sent, 8, $order));
    }

    private static function reads(string $class): CodebaseReads
    {
        $reads = new CodebaseReads();
        $reads->record(1, 0, $class);

        return $reads;
    }

    /**
     * @return list<null|bool|int|float|string|list<mixed>>
     */
    private static function written(SentReads $sent, int $generation, CodebaseReads $reads): array
    {
        $writer = new ValueWriter();
        $sent->write($writer, $generation, $reads);

        return $writer->finish();
    }
}
