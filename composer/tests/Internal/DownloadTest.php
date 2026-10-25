<?php

declare(strict_types=1);

namespace Mago\Tests\Internal;

use PHPUnit\Framework\TestCase;
use RuntimeException;

use function fclose;
use function file_get_contents;
use function file_put_contents;
use function fsockopen;
use function is_resource;
use function Mago\Internal\download_with_curl;
use function Mago\Internal\download_with_fopen;
use function proc_close;
use function proc_open;
use function proc_terminate;
use function putenv;
use function stream_socket_get_name;
use function stream_socket_server;
use function strrpos;
use function substr;
use function sys_get_temp_dir;
use function tempnam;
use function unlink;
use function urlencode;
use function usleep;

use const PHP_BINARY;

/**
 * A GitHub release download answers with a redirect to another host, objects.githubusercontent.com. The token in
 * GITHUB_TOKEN goes only to the host the download starts at, so the redirect's host never receives it. One server
 * stands in for both hosts: 127.0.0.1 redirects to localhost, and localhost answers with the Authorization header
 * each of them received.
 */
final class DownloadTest extends TestCase
{
    private const ROUTER = <<<'PHP'
        <?php
        $authorization = $_SERVER['HTTP_AUTHORIZATION'] ?? 'none';
        if (isset($_GET['to'])) {
            header('Location: ' . $_GET['to'] . '?first=' . urlencode($authorization), true, 302);
            return;
        }
        echo "first host: {$_GET['first']}, redirect host: {$authorization}";
        PHP;

    /** @var resource|null */
    private static mixed $server = null;

    private static string $router = '';

    private static string $port = '';

    public static function setUpBeforeClass(): void
    {
        self::$router = (string) tempnam(sys_get_temp_dir(), 'mago-router');
        file_put_contents(self::$router, self::ROUTER);
        self::$port = self::freePort();
        $output = ['file', '/dev/null', 'w'];
        $pipes = [];
        $server = proc_open(
            [PHP_BINARY, '-S', '127.0.0.1:' . self::$port, self::$router],
            [1 => $output, 2 => $output],
            $pipes,
        );
        if (!is_resource($server)) {
            throw new RuntimeException('Unable to start the redirecting test server.');
        }

        self::$server = $server;
        self::awaitServer();
    }

    public static function tearDownAfterClass(): void
    {
        if (is_resource(self::$server)) {
            proc_terminate(self::$server);
            proc_close(self::$server);
        }

        unlink(self::$router);
    }

    protected function tearDown(): void
    {
        putenv('GITHUB_TOKEN');
    }

    public function testCurlSendsTheTokenToTheFirstHostAndNotToTheRedirectsHost(): void
    {
        self::assertSame(
            'first host: Bearer redirect-test-token, redirect host: none',
            self::download(download_with_curl(...)),
        );
    }

    public function testTheStreamWrapperSendsTheTokenToTheFirstHostAndNotToTheRedirectsHost(): void
    {
        self::assertSame(
            'first host: Bearer redirect-test-token, redirect host: none',
            self::download(download_with_fopen(...)),
        );
    }

    /**
     * Downloads through the redirect with the token set, and returns what the redirect's host answered.
     *
     * @param \Closure(string, string): void $download
     */
    private static function download(\Closure $download): string
    {
        putenv('GITHUB_TOKEN=redirect-test-token');
        $destination = (string) tempnam(sys_get_temp_dir(), 'mago-download');
        try {
            $redirect = 'http://localhost:' . self::$port . '/asset';
            $download('http://127.0.0.1:' . self::$port . '/?to=' . urlencode($redirect), $destination);

            return (string) file_get_contents($destination);
        } finally {
            unlink($destination);
        }
    }

    private static function freePort(): string
    {
        $socket = stream_socket_server('tcp://127.0.0.1:0');
        if ($socket === false) {
            throw new RuntimeException('Unable to find a free port for the redirecting test server.');
        }

        $address = (string) stream_socket_get_name($socket, false);
        fclose($socket);

        return substr($address, (int) strrpos($address, ':') + 1);
    }

    /**
     * Waits up to five seconds for the server to accept a connection.
     *
     * @mago-expect lint:no-error-control-operator
     */
    private static function awaitServer(): void
    {
        for ($attempt = 0; $attempt < 500; $attempt++) {
            $connection = @fsockopen('127.0.0.1', (int) self::$port);
            if ($connection !== false) {
                fclose($connection);

                return;
            }

            usleep(10_000);
        }

        throw new RuntimeException(
            'The redirecting test server on port ' . self::$port . ' never accepted a connection.',
        );
    }
}
