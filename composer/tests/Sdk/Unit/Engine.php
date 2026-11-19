<?php

declare(strict_types=1);

namespace Mago\Engine;

use Mago\Tests\Sdk\Unit\Mago;

/**
 * The function mago's engine gives the PHP it runs, which these tests answer as mago does.
 */
function ask(string $question, string $payload): ?string
{
    return Mago::ask($question, $payload);
}
