<?php

declare(strict_types=1);

/**
 * @param iterable<int, int|string|float|null> $src
 *
 * @return Generator<int, int|string|float|bool>
 *
 * @mago-expect analysis:yield-from-invalid-value-type
 */
function relay(iterable $src): Generator
{
    yield from $src;
}
