<?php

declare(strict_types=1);

/**
 * @param iterable<5|6|7|8, int> $src
 *
 * @return Generator<1|2|3|4, int>
 *
 * @mago-expect analysis:yield-from-invalid-key-type
 */
function relay(iterable $src): Generator
{
    yield from $src;
}
