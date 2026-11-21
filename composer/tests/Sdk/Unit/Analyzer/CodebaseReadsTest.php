<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use Mago\Sdk\Internal\Analyzer\CodebaseReads;
use PHPUnit\Framework\TestCase;

final class CodebaseReadsTest extends TestCase
{
    /**
     * A query of several names reads each of them once, as a query per name does, and two lists of the same reads in
     * the same order share their key while another list does not.
     */
    public function testAQueryOfSeveralNamesReadsWhatAQueryPerNameReads(): void
    {
        $together = new CodebaseReads();
        $together->recordAll(1, 0, ['App\Order', 'App\Line', 'App\Order']);
        $together->record(3, 0, 'LIMIT');
        $apart = new CodebaseReads();
        $apart->record(1, 0, 'App\Order');
        $apart->record(1, 0, 'App\Line');
        $apart->record(3, 0, 'LIMIT');
        $other = new CodebaseReads();
        $other->record(1, 0, 'App\Order');

        self::assertSame([[1, 0, 'App\Order'], [1, 0, 'App\Line'], [3, 0, 'LIMIT']], $together->reads());
        self::assertSame($apart->reads(), $together->reads());
        self::assertSame($apart->key(), $together->key());
        self::assertNotSame($other->key(), $together->key());
    }
}
