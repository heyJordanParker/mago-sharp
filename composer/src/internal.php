<?php

declare(strict_types=1);

namespace Mago\Internal;

use Composer\InstalledVersions;
use PharData;
use RuntimeException;
use Throwable;
use ZipArchive;

use function array_map;
use function basename;
use function chmod;
use function curl_error;
use function curl_exec;
use function curl_getinfo;
use function curl_init;
use function curl_setopt;
use function dirname;
use function escapeshellarg;
use function explode;
use function extension_loaded;
use function fclose;
use function file;
use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function flock;
use function fopen;
use function fprintf;
use function fwrite;
use function getenv;
use function hash_equals;
use function hash_file;
use function implode;
use function ini_get;
use function is_dir;
use function is_resource;
use function is_string;
use function ltrim;
use function mkdir;
use function number_format;
use function php_uname;
use function preg_match;
use function proc_close;
use function proc_get_status;
use function proc_open;
use function shell_exec;
use function str_contains;
use function stream_context_create;
use function stream_get_contents;
use function stream_get_meta_data;
use function stripos;
use function strtolower;
use function substr;
use function sys_get_temp_dir;
use function trim;
use function unlink;
use function usleep;

use const CURLINFO_HTTP_CODE;
use const CURLOPT_FILE;
use const CURLOPT_FOLLOWLOCATION;
use const CURLOPT_HTTPHEADER;
use const CURLOPT_NOPROGRESS;
use const CURLOPT_PROGRESSFUNCTION;
use const CURLOPT_UNRESTRICTED_AUTH;
use const CURLOPT_USERAGENT;
use const FILE_IGNORE_NEW_LINES;
use const LOCK_EX;
use const LOCK_UN;
use const STDERR;

// 10ms prevents spin-locking while keeping worst-case overhead under 10ms.
const STATUS_CHECK_INTERVAL = 10_000;

/**
 * Execute a closure while holding an exclusive file lock.
 *
 * Blocks until the lock is acquired. The lock is always released when the
 * closure returns or throws, even on fatal errors.
 *
 * @template T
 *
 * @param string $lockFile Path to the lock file (created if it does not exist).
 * @param \Closure(): T $callback The work to perform while the lock is held.
 *
 * @throws RuntimeException If the lock file cannot be opened.
 *
 * @return T
 *
 * @internal
 */
function locked(string $lockFile, \Closure $callback): mixed
{
    $handle = fopen($lockFile, 'c');
    if ($handle === false) {
        throw new RuntimeException("Unable to create lock file: {$lockFile}");
    }

    flock($handle, LOCK_EX);

    try {
        return $callback();
    } finally {
        flock($handle, LOCK_UN);
        fclose($handle);
    }
}

/**
 * Get the installed mago version from Composer metadata, without the `v` of php-sharp's release tags.
 *
 * @throws RuntimeException If the version cannot be determined.
 *
 * @return string The package version (e.g., "1.10.0").
 *
 * @internal
 */
function get_version(): string
{
    $version = InstalledVersions::getPrettyVersion('heyjordanparker/mago-sharp');
    if ($version === null) {
        throw new RuntimeException('Could not determine mago package version.');
    }

    return ltrim($version, 'v');
}

/**
 * Detect the CPU architecture from the system.
 *
 * Maps the raw `php_uname('m')` value to a normalized Rust target architecture name.
 * Only architectures with pre-built release binaries are supported.
 *
 * Supported: x86_64, aarch64, armv7, arm (v5/v6).
 *
 * @throws RuntimeException If the architecture is not supported.
 *
 * @return string Normalized architecture (e.g., "x86_64", "aarch64", "armv7", "arm").
 *
 * @internal
 */
function detect_architecture(): string
{
    $raw = strtolower(php_uname('m'));

    return match ($raw) {
        'x86_64', 'amd64' => 'x86_64',
        'arm64', 'aarch64' => 'aarch64',
        'armv7l' => 'armv7',
        'armv6l', 'armv5tel', 'armv5l' => 'arm',
        default => throw new RuntimeException(
            "Unsupported architecture: {$raw}. Pre-built binaries are available for x86_64, aarch64, armv7, and arm. "
            . 'For other architectures, compile mago from source: https://github.com/heyJordanParker/php-sharp/tree/master/sharp/mago',
        ),
    };
}

/**
 * Detect the C library variant on Linux (glibc or musl).
 *
 * Uses `ldd --version` output to distinguish between glibc and musl.
 * Falls back to musl if `ldd` is not found (common in minimal containers).
 *
 * @return string "musl" or "gnu".
 *
 * @internal
 */
function detect_linux_libc(): string
{
    $lddPath = trim((string) shell_exec('command -v ldd 2>/dev/null'));
    if ($lddPath === '') {
        return 'musl';
    }

    $lddVersion = (string) shell_exec('ldd --version 2>&1');

    return str_contains($lddVersion, 'musl') ? 'musl' : 'gnu';
}

/**
 * Detect the ARM float ABI from /proc/cpuinfo.
 *
 * Checks whether the CPU supports hardware floating point (hard float).
 *
 * @return bool True if the system uses hard float ABI.
 *
 * @internal
 */
function detect_arm_hard_float(): bool
{
    return str_contains((string) file_get_contents('/proc/cpuinfo'), 'hard');
}

/**
 * Build the Linux OS suffix for the target triple.
 *
 * Combines the C library variant (glibc/musl) with the appropriate ABI suffix
 * for the given architecture. Only combinations with pre-built binaries are supported.
 *
 * Released Linux targets:
 *   x86_64:  gnu, musl
 *   aarch64: gnu, musl
 *   armv7:   gnueabihf, musleabihf (hard float only)
 *   arm:     gnueabi, gnueabihf, musleabi, musleabihf
 *
 * @param string $architecture Normalized architecture name.
 * @param string $libc C library variant ("gnu" or "musl").
 *
 * @throws RuntimeException If the architecture/libc combination has no pre-built binary.
 *
 * @return string The OS suffix (e.g., "gnu", "musl", "gnueabihf", "musleabi").
 *
 * @internal
 */
function build_linux_suffix(string $architecture, string $libc): string
{
    $hardFloat = match ($architecture) {
        'arm', 'armv7' => namespace\detect_arm_hard_float(),
        default => false,
    };

    if ($libc === 'musl') {
        return match ($architecture) {
            'x86_64', 'aarch64' => 'musl',
            'armv7' => 'musleabihf',
            'arm' => $hardFloat ? 'musleabihf' : 'musleabi',
            default => throw new RuntimeException("No pre-built musl binary for architecture: {$architecture}"),
        };
    }

    return match ($architecture) {
        'x86_64', 'aarch64' => 'gnu',
        'armv7' => 'gnueabihf',
        'arm' => $hardFloat ? 'gnueabihf' : 'gnueabi',
        default => throw new RuntimeException("No pre-built glibc binary for architecture: {$architecture}"),
    };
}

/**
 * Detect the operating system and build platform metadata.
 *
 * Returns all platform-specific values needed to construct the download URL
 * and locate the extracted binary. Only OS/architecture combinations with
 * pre-built release binaries are supported.
 *
 * Released OS targets:
 *   Windows: x86_64 only (msvc)
 *   macOS:   x86_64, aarch64
 *   Linux:   x86_64, aarch64, armv7, arm
 *   FreeBSD: x86_64 only
 *
 * @param string $architecture Normalized architecture name.
 *
 * @throws RuntimeException If the OS/architecture combination is not supported.
 *
 * @return array{os: string, vendor: string, suffix: string, extension: string}
 *   - os: Normalized OS name (e.g., "linux", "darwin", "windows")
 *   - vendor: Target vendor (e.g., "unknown", "apple", "pc")
 *   - suffix: OS/ABI suffix for the target triple (e.g., "gnu", "musl", "msvc")
 *   - extension: Executable file extension (e.g., "", ".exe")
 *
 * @internal
 */
function detect_platform(string $architecture): array
{
    $os = strtolower(php_uname('s'));

    return match ($os) {
        'windows nt' => match ($architecture) {
            'x86_64' => [
                'os' => 'windows',
                'vendor' => 'pc',
                'suffix' => 'msvc',
                'extension' => '.exe',
            ],
            default => throw new RuntimeException(
                "No pre-built Windows binary for architecture: {$architecture}. Only x86_64 is supported.",
            ),
        },
        'darwin' => match ($architecture) {
            'x86_64', 'aarch64' => [
                'os' => 'darwin',
                'vendor' => 'apple',
                'suffix' => '',
                'extension' => '',
            ],
            default => throw new RuntimeException(
                "No pre-built macOS binary for architecture: {$architecture}. Only x86_64 and aarch64 are supported.",
            ),
        },
        'linux' => [
            'os' => 'linux',
            'vendor' => 'unknown',
            'suffix' => namespace\build_linux_suffix($architecture, namespace\detect_linux_libc()),
            'extension' => '',
        ],
        'freebsd' => match ($architecture) {
            'x86_64' => [
                'os' => 'freebsd',
                'vendor' => 'unknown',
                'suffix' => '',
                'extension' => '',
            ],
            default => throw new RuntimeException(
                "No pre-built FreeBSD binary for architecture: {$architecture}. Only x86_64 is supported.",
            ),
        },
        default => throw new RuntimeException(
            "Unsupported operating system: {$os}. Pre-built binaries are available for Windows, macOS, Linux, and FreeBSD. "
            . 'For other platforms, compile mago from source: https://github.com/heyJordanParker/php-sharp/tree/master/sharp/mago',
        ),
    };
}

/**
 * Build the Rust target triple from platform components.
 *
 * @param string $arch Architecture (e.g., "x86_64").
 * @param string $vendor Vendor (e.g., "unknown", "apple").
 * @param string $os OS name (e.g., "linux", "darwin").
 * @param string $suffix ABI suffix (e.g., "gnu", "musl", "msvc", or "").
 *
 * @return string The target triple (e.g., "x86_64-unknown-linux-gnu", "aarch64-apple-darwin").
 *
 * @internal
 */
function build_target_triple(string $arch, string $vendor, string $os, string $suffix): string
{
    if ($suffix !== '') {
        return "{$arch}-{$vendor}-{$os}-{$suffix}";
    }

    return "{$arch}-{$vendor}-{$os}";
}

/**
 * Get the archive file extension for the given platform.
 *
 * Windows MSVC builds are packaged as `.zip`, all other platforms use `.tar.gz`.
 *
 * @return string ".zip" or ".tar.gz".
 *
 * @internal
 */
function get_archive_extension(string $os, string $suffix): string
{
    return $os === 'windows' && $suffix === 'msvc' ? '.zip' : '.tar.gz';
}

/**
 * Build the GitHub release download URL for a given version and target.
 *
 * @param string $version Package version (e.g., "1.10.0").
 * @param string $storageDir Directory name inside the archive (e.g., "mago-1.10.0-x86_64-unknown-linux-gnu").
 * @param string $archiveExtension Archive extension (".zip" or ".tar.gz").
 *
 * @return string Full download URL.
 *
 * @internal
 */
function build_download_url(string $version, string $storageDir, string $archiveExtension): string
{
    return "https://github.com/heyJordanParker/php-sharp/releases/download/v{$version}/{$storageDir}{$archiveExtension}";
}

/**
 * Read a GitHub API token from the environment.
 *
 * Mirrors `mago self-update`: checks `GITHUB_TOKEN` first, then `GH_TOKEN`, returning the
 * first non-empty value. Sending this token with download requests avoids GitHub's anonymous
 * rate limits in CI or developer environments that are already authenticated.
 *
 * @return null|non-empty-string The token, or null when neither variable is set.
 *
 * @internal
 */
function get_github_token(): ?string
{
    foreach (['GITHUB_TOKEN', 'GH_TOKEN'] as $variable) {
        $value = getenv($variable);
        if (is_string($value) && $value !== '') {
            return $value;
        }
    }

    return null;
}

/**
 * Download a file from a URL using the best available method.
 *
 * Prefers the curl extension (with progress display) and falls back to
 * `file_get_contents` when `allow_url_fopen` is enabled.
 *
 * @throws RuntimeException If the download fails or no download method is available.
 *
 * @internal
 */
function download(string $url, string $destination): void
{
    if (extension_loaded('curl')) {
        namespace\download_with_curl($url, $destination);

        return;
    }

    if (ini_get('allow_url_fopen')) {
        namespace\download_with_fopen($url, $destination);

        return;
    }

    throw new RuntimeException(
        'Unable to download mago binary. Either install the PHP curl extension or set allow_url_fopen=1 in php.ini.',
    );
}

/**
 * Download a file using the curl extension with a progress bar.
 *
 * @throws RuntimeException If the download fails or the server returns an error status.
 *
 * @internal
 */
function download_with_curl(string $url, string $destination): void
{
    $ch = curl_init($url);
    $fh = fopen($destination, 'w');
    curl_setopt($ch, CURLOPT_FOLLOWLOCATION, true);
    curl_setopt($ch, CURLOPT_FILE, $fh);
    curl_setopt($ch, CURLOPT_NOPROGRESS, false);
    curl_setopt($ch, CURLOPT_USERAGENT, 'mago-composer/' . namespace\get_version());

    // The release download redirects to objects.githubusercontent.com. curl 7.58.0 and later send a custom
    // Authorization header only to the host the request starts at, unless CURLOPT_UNRESTRICTED_AUTH is set, so
    // the token never reaches the redirect's host.
    $token = namespace\get_github_token();
    if ($token !== null) {
        curl_setopt($ch, CURLOPT_HTTPHEADER, ['Authorization: Bearer ' . $token]);
        curl_setopt($ch, CURLOPT_UNRESTRICTED_AUTH, false);
    }

    curl_setopt($ch, CURLOPT_PROGRESSFUNCTION, function (mixed $_resource, int $dlSize, int $dlNow): int {
        if ($dlSize > 0) {
            $pct = (int) (($dlNow / $dlSize) * 100);
            $dlMb = number_format($dlNow / 1_048_576, 1);
            $totalMb = number_format($dlSize / 1_048_576, 1);
            fprintf(STDERR, "\r  %s / %s MB (%d%%)", $dlMb, $totalMb, $pct);
        }

        return 0;
    });

    $success = curl_exec($ch);
    /** @var int<100, 599> */
    $statusCode = curl_getinfo($ch, CURLINFO_HTTP_CODE);
    $error = curl_error($ch);
    fclose($fh);

    if (!$success || $statusCode >= 400) {
        unlink($destination);

        throw new RuntimeException("Failed to download mago binary (HTTP {$statusCode}): {$error}\nURL: {$url}");
    }

    fprintf(STDERR, "\n");
}

/**
 * Download a file using PHP's HTTP stream wrapper (requires `allow_url_fopen`).
 *
 * The release download redirects to objects.githubusercontent.com, and the wrapper sends a context's headers to
 * every host it follows. So this follows each redirect itself, and only the first request carries the GitHub
 * token, as curl sends it.
 *
 * @throws RuntimeException If the download fails or redirects more than 20 times.
 *
 * @internal
 */
function download_with_fopen(string $url, string $destination): void
{
    $token = namespace\get_github_token();
    for ($redirects = 0; $redirects <= 20; $redirects++) {
        $stream = namespace\open_url($url, $redirects === 0 ? $token : null);
        $location = namespace\redirect_location($stream);
        if ($location === null) {
            file_put_contents($destination, $stream);
            fclose($stream);

            return;
        }

        fclose($stream);
        $url = $location;
    }

    throw new RuntimeException("Failed to download mago binary: it redirected more than 20 times.\nURL: {$url}");
}

/**
 * Open a URL with PHP's HTTP stream wrapper, without following a redirect.
 *
 * @param null|non-empty-string $token The GitHub token to send, or null to send none.
 *
 * @throws RuntimeException If the request fails.
 *
 * @return resource
 *
 * @internal
 */
function open_url(string $url, ?string $token): mixed
{
    $headers = ['User-Agent: mago-composer/' . namespace\get_version()];
    if ($token !== null) {
        $headers[] = 'Authorization: Bearer ' . $token;
    }

    $context = stream_context_create([
        'http' => [
            'method' => 'GET',
            'header' => implode("\r\n", $headers),
            'follow_location' => 0,
        ],
    ]);

    $stream = fopen($url, 'rb', false, $context);
    if ($stream === false) {
        throw new RuntimeException("Failed to download mago binary.\nURL: {$url}");
    }

    return $stream;
}

/**
 * Read the URL a response redirects to.
 *
 * @param resource $stream A stream `open_url` opened.
 *
 * @throws RuntimeException If a redirect names no Location.
 *
 * @return null|string The Location of a 3xx response, or null for any other response.
 *
 * @internal
 */
function redirect_location(mixed $stream): ?string
{
    /** @var list<string> $headers */
    $headers = stream_get_meta_data($stream)['wrapper_data'] ?? [];
    if (preg_match('{^HTTP/\S+ 3\d\d\b}', $headers[0] ?? '') !== 1) {
        return null;
    }

    foreach ($headers as $header) {
        if (stripos($header, 'Location:') === 0) {
            return trim(substr($header, 9));
        }
    }

    throw new RuntimeException("The download answered {$headers[0]} with no Location.");
}

/**
 * Read the SHA-256 a heyjordanparker/mago-sharp release pins for one of its release's archives.
 *
 * release.yml writes `composer/sha256sums.txt`, in `sha256sum`'s format, into the package version it publishes, so
 * the package version pins the exact binary. A hash read from the release itself would not catch a replaced asset.
 *
 * @param string $checksumFile Path to the package's `sha256sums.txt`.
 * @param string $archiveName The archive's file name (e.g., "mago-1.10.0-x86_64-unknown-linux-gnu.tar.gz").
 *
 * @throws RuntimeException If the file does not exist or pins no SHA-256 for the archive.
 *
 * @return string The archive's SHA-256, in lowercase hex.
 *
 * @internal
 */
function get_archive_checksum(string $checksumFile, string $archiveName): string
{
    $lines = file_exists($checksumFile) ? file($checksumFile, FILE_IGNORE_NEW_LINES) : false;
    if ($lines === false) {
        throw new RuntimeException(
            "{$checksumFile} does not exist, so no mago download can be verified. Only a heyjordanparker/mago-sharp "
            . 'release ships it: require a release version.',
        );
    }

    foreach ($lines as $line) {
        [$checksum, $name] = explode('  ', $line, 2) + ['', ''];
        if ($name === $archiveName) {
            return $checksum;
        }
    }

    throw new RuntimeException(
        "{$checksumFile} pins no SHA-256 for {$archiveName}, so its download cannot be verified.",
    );
}

/**
 * Extract an archive to a destination directory, if its SHA-256 is the one the release pins.
 *
 * Supports `.zip` (via ZipArchive) and `.tar.gz` (via PharData). The archive is deleted either way.
 *
 * @param string $checksum The SHA-256 `get_archive_checksum` read for the archive.
 *
 * @throws RuntimeException If the archive's SHA-256 differs, or the archive cannot be opened or extracted.
 *
 * @internal
 */
function extract_archive(string $archiveFile, string $checksum, string $destination, string $archiveExtension): void
{
    $actual = (string) hash_file('sha256', $archiveFile);
    if (!hash_equals($checksum, $actual)) {
        unlink($archiveFile);

        throw new RuntimeException(
            'The downloaded '
            . basename($archiveFile)
            . " has SHA-256 {$actual}, and heyjordanparker/mago-sharp pins {$checksum} for it, so it is not the "
            . 'archive the release built. It was deleted, and nothing was extracted.',
        );
    }

    if ($archiveExtension === '.zip') {
        $zip = new ZipArchive();
        if ($zip->open($archiveFile) !== true) {
            unlink($archiveFile);

            throw new RuntimeException('Failed to open zip archive.');
        }

        $zip->extractTo($destination);
        $zip->close();

        unlink($archiveFile);
        return;
    }

    $phar = new PharData($archiveFile);
    $phar->extractTo($destination);
    unlink($archiveFile);
}

/**
 * Ensure the mago binary is available, downloading it if necessary.
 *
 * Checks if the binary already exists at the expected path. If not, downloads
 * the appropriate release archive, checks its SHA-256 against the one the package
 * pins, extracts it, and sets executable permissions.
 *
 * @param string $version Package version.
 * @param string $triple Target triple (e.g., "x86_64-unknown-linux-gnu").
 * @param string $executableExtension Executable extension ("" or ".exe").
 * @param string $archiveExtension Archive extension (".zip" or ".tar.gz").
 * @param string $binDir Base directory for storing binaries, the package's `composer/bin`. The package's
 *   `sha256sums.txt` is in its parent directory.
 *
 * @throws RuntimeException If the download, checksum, extraction, or binary verification fails.
 *
 * @return string Path to the mago executable.
 *
 * @internal
 */
function ensure_binary(
    string $version,
    string $triple,
    string $executableExtension,
    string $archiveExtension,
    string $binDir,
): string {
    $storageDir = "mago-{$version}-{$triple}";
    $releaseDir = "{$binDir}/{$version}";
    $executablePath = "{$releaseDir}/{$storageDir}/mago{$executableExtension}";
    $checksumFile = dirname($binDir) . '/sha256sums.txt';

    if (file_exists($executablePath)) {
        return $executablePath;
    }

    if (!is_dir($releaseDir)) {
        mkdir($releaseDir, 0o755, true);
    }

    // Lock per version+triple so different targets don't block each other.
    $lockFile = "{$releaseDir}/.mago-{$triple}.lock";

    return namespace\locked($lockFile, static function () use (
        $version,
        $triple,
        $executableExtension,
        $archiveExtension,
        $releaseDir,
        $storageDir,
        $executablePath,
        $checksumFile,
    ): string {
        // Re-check after acquiring lock: another process may have completed the download. (double-checked locking)
        if (file_exists($executablePath)) {
            return $executablePath;
        }

        $checksum = namespace\get_archive_checksum($checksumFile, "{$storageDir}{$archiveExtension}");
        $archiveFile = "{$releaseDir}/{$storageDir}{$archiveExtension}";
        $url = namespace\build_download_url($version, $storageDir, $archiveExtension);

        fprintf(STDERR, "Downloading mago %s for %s...\n", $version, $triple);
        namespace\download($url, $archiveFile);
        fprintf(STDERR, "Downloaded.\n");

        namespace\extract_archive($archiveFile, $checksum, $releaseDir, $archiveExtension);

        if (!file_exists($executablePath)) {
            throw new RuntimeException("Expected binary not found after extraction at {$executablePath}");
        }

        // Ensure binary is executable on Unix
        if ($executableExtension === '') {
            chmod($executablePath, 0o755);
        }

        return $executablePath;
    });
}

/**
 * Ensure the editor JSON schema is available next to the package.
 *
 * Writes `schema.json` to the package root (`vendor/heyjordanparker/mago-sharp/schema.json`
 * once installed) so a project can reference a local, version-matched schema instead of a
 * version-pinned URL:
 *
 *     #:schema vendor/heyjordanparker/mago-sharp/schema.json
 *
 * The schema is produced by the just-installed binary, so it always matches the version in
 * use. Generation is best-effort: any failure (read-only vendor dir, binary error, ...) is
 * swallowed so it never breaks the user's actual command. Each installed version ships in its
 * own package directory, so a plain existence check is enough - no version marker is needed.
 *
 * @param string $executablePath Path to the mago binary.
 * @param string $packageRoot The package root directory.
 *
 * @mago-expect lint:no-empty-catch-clause
 *
 * @internal
 */
function ensure_schema(string $executablePath, string $packageRoot): void
{
    $schemaPath = "{$packageRoot}/schema.json";
    if (file_exists($schemaPath)) {
        return;
    }

    try {
        $lockFile = "{$packageRoot}/.mago-schema.lock";

        namespace\locked($lockFile, static function () use ($executablePath, $schemaPath): void {
            // Re-check after acquiring the lock: another process may have written it, somehow.
            if (file_exists($schemaPath)) {
                return;
            }

            $schema = namespace\capture_schema($executablePath);
            if ($schema !== null) {
                file_put_contents($schemaPath, $schema);
            }
        });
    } catch (Throwable) {
        // Best-effort: a missing local schema must never block running mago.
    }
}

/**
 * Run `mago config --schema` and capture its JSON output.
 *
 * Runs from the system temp directory so the project's own (possibly invalid) `mago.toml` is
 * not sourced - the schema is static, so it does not depend on any configuration. Standard
 * error (debug logs) is discarded; only stdout carries the schema.
 *
 * @param string $executablePath Path to the mago binary.
 *
 * @return null|non-empty-string The schema JSON, or null when generation fails.
 *
 * @internal
 */
function capture_schema(string $executablePath): ?string
{
    [$process, $stdout] = namespace\open_schema_process($executablePath);
    if ($process === null || $stdout === null) {
        return null;
    }

    $schema = stream_get_contents($stdout);
    fclose($stdout);

    if (proc_close($process) !== 0) {
        return null;
    }

    if (!is_string($schema) || $schema === '') {
        return null;
    }

    return $schema;
}

/**
 * Spawn `mago config --schema` from the system temp directory with stdout piped.
 *
 * @param string $executablePath Path to the mago binary.
 *
 * @return array{0: resource|null, 1: resource|null} The process handle and its stdout pipe, or
 *   `[null, null]` when the process could not be started.
 *
 * @internal
 */
function open_schema_process(string $executablePath): array
{
    $nullDevice = DIRECTORY_SEPARATOR === '\\' ? 'NUL' : '/dev/null';

    $pipes = [];
    $process = proc_open(
        escapeshellarg($executablePath) . ' config --schema',
        [
            0 => ['file', $nullDevice, 'r'],
            1 => ['pipe', 'w'],
            2 => ['file', $nullDevice, 'w'],
        ],
        $pipes,
        sys_get_temp_dir(),
    );

    if (!is_resource($process)) {
        return [null, null];
    }

    $stdout = $pipes[1] ?? null;
    if (!is_resource($stdout)) {
        proc_close($process);

        return [null, null];
    }

    return [$process, $stdout];
}

/**
 * Execute the mago binary, forwarding stdin/stdout/stderr.
 *
 * This function does not return; it exits with the binary's exit code.
 *
 * @param string $executablePath Path to the mago binary.
 * @param list<string> $args Command-line arguments to pass.
 *
 * @return never
 *
 * @mago-expect lint:no-error-control-operator
 *
 * @internal
 */
function execute(string $executablePath, array $args): never
{
    $command = escapeshellarg($executablePath);
    if ($args !== []) {
        $command .= ' ' . implode(' ', array_map(escapeshellarg(...), $args));
    }

    $pipes = [];
    $process = @proc_open(
        $command,
        [
            0 => ['file', 'php://stdin', 'r'],
            1 => ['file', 'php://stdout', 'w'],
            2 => ['file', 'php://stderr', 'w'],
        ],
        $pipes,
    );

    if (!is_resource($process)) {
        fwrite(STDERR, "Error: Unable to start mago process.\n");
        exit(1);
    }

    do {
        usleep(STATUS_CHECK_INTERVAL);
        $status = proc_get_status($process);
    } while ($status['running']);

    $exitCode = $status['exitcode'];
    if ($status['signaled']) {
        $exitCode = $status['termsig'] + 128;
    }

    proc_close($process);
    exit($exitCode);
}
