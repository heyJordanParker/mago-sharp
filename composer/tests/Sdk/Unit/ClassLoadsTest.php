<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Mago\Sdk\Internal\ClassLoads;
use Mago\Sdk\Internal\Protocol\ValueReader;
use Mago\Sdk\Internal\Protocol\ValueWriter;
use PHPUnit\Framework\TestCase;

use function class_exists;

final class ClassLoadsTest extends TestCase
{
    /**
     * A class the worker was asked for and never declared is in its log, by its lowercase name, so the worker starts
     * again once the class's compiled file changes, and each response sends only the entries Mago has not had.
     */
    public function testAClassTheWorkerNeverDeclaredIsLoggedAndSentOnce(): void
    {
        $loads = new ClassLoads();
        $before = $loads->end();
        self::assertFalse(class_exists('App\Missing\Order'));
        $end = $loads->end();

        self::assertSame($before + 1, $end);
        self::assertTrue($loads->holdsAny(['app\missing\order']));
        self::assertFalse($loads->holdsAny(['app\missing\other']));

        $first = new ValueWriter();
        $loads->write($first, $end);
        $reader = new ValueReader($first->finish());
        $reader->readU64();
        self::assertSame(0, $reader->readU32());
        self::assertSame($end, $reader->readCount(1_000));

        $again = new ValueWriter();
        $loads->write($again, $end);
        $reader = new ValueReader($again->finish());
        $reader->readU64();
        self::assertSame($end, $reader->readU32(), 'a later response starts where the last one ended');
        self::assertSame(0, $reader->readCount(1_000));
    }
}
