<?php

declare(strict_types=1);

/**
 * @return Generator<int, int>
 *
 * @mago-expect analysis:non-existent-function
 * @mago-expect analysis:unknown-yield-from-iterator-type
 */
function relay(): Generator
{
    yield from undefined_function();
}
