<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use RuntimeException;

use function stream_socket_pair;

use const PHP_OS_FAMILY;
use const STREAM_IPPROTO_IP;
use const STREAM_PF_INET;
use const STREAM_PF_UNIX;
use const STREAM_SOCK_STREAM;

/**
 * Two connected streams for SDK tests: what one writes, the other reads.
 */
final class StreamPair
{
    /**
     * @return array{resource, resource}
     */
    public static function create(): array
    {
        $domain = PHP_OS_FAMILY === 'Windows' ? STREAM_PF_INET : STREAM_PF_UNIX;
        $streams = stream_socket_pair($domain, STREAM_SOCK_STREAM, STREAM_IPPROTO_IP);
        if ($streams === false) {
            throw new RuntimeException('Unable to create SDK test streams.');
        }

        return $streams;
    }
}
