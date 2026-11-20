<?php

declare(strict_types=1);

namespace Mago\Engine;

use Mago\Tests\Sdk\Unit\Mago;

use function count;
use function get_declared_classes;
use function get_declared_interfaces;
use function get_declared_traits;

/**
 * The question mago's engine lets the PHP it runs ask, which these tests answer as mago does: a `query` of a message
 * of values, which mago answers with one, or a `progress` of the unit a request starts.
 *
 * @param string|list<null|bool|int|float|string|list<mixed>> $payload
 *
 * @return null|list<null|bool|int|float|string|list<mixed>>
 */
function ask(string $question, string|array $payload): ?array
{
    return Mago::ask($question, $payload);
}

/**
 * How many classes, interfaces, traits and enums PHP has declared, as mago's engine counts its class table.
 */
function classCount(): int
{
    return count(get_declared_classes()) + count(get_declared_interfaces()) + count(get_declared_traits());
}
