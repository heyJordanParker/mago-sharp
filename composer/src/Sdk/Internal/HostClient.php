<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal;

use Mago\Sdk\Exception\ProtocolException;
use RuntimeException;

use function is_string;
use function Mago\Engine\ask;

/**
 * Asks mago, which runs the worker in its own process, what a hook or provider needs while it runs.
 *
 * @internal
 */
final class HostClient
{
    /**
     * Sends mago a codebase query and returns its answer.
     *
     * @throws ProtocolException when mago refuses the query.
     */
    public function request(string $payload): string
    {
        try {
            $answer = ask('query', $payload);
        } catch (RuntimeException $exception) {
            throw new ProtocolException($exception->getMessage(), 0, $exception);
        }

        if (!is_string($answer)) {
            throw new ProtocolException('mago answered a codebase query with no payload.');
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
