<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal;

use Mago\Sdk\Exception\ProtocolException;
use RuntimeException;

use function is_array;
use function Mago\Engine\ask;

/**
 * Asks mago, which runs the worker in its own process, what a hook or provider needs while it runs.
 *
 * @internal
 */
final class HostClient
{
    /**
     * Sends mago a codebase query, a message of values, and returns its answer, which is one too.
     *
     * @param list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return list<null|bool|int|float|string|list<mixed>>
     *
     * @throws ProtocolException when mago refuses the query.
     */
    public function request(array $payload): array
    {
        try {
            $answer = ask('query', $payload);
        } catch (RuntimeException $exception) {
            throw new ProtocolException($exception->getMessage(), 0, $exception);
        }

        if (!is_array($answer)) {
            throw new ProtocolException('mago answered a codebase query with no message.');
        }

        return $answer;
    }

    /**
     * Tells mago that the request starts `$running`, such as "the hooks of app/User.php", which starts its deadline
     * again. An error for a deadline the request then misses says it made no progress on `$running`.
     */
    public function progress(string $running): void
    {
        ask('progress', $running);
    }
}
