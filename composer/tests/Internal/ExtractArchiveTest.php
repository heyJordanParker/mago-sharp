<?php

declare(strict_types=1);

namespace Mago\Tests\Internal;

use FilesystemIterator;
use PharData;
use PHPUnit\Framework\TestCase;
use RecursiveDirectoryIterator;
use RecursiveIteratorIterator;
use RuntimeException;
use SplFileInfo;

use function file_exists;
use function file_get_contents;
use function file_put_contents;
use function gzencode;
use function hash;
use function hash_file;
use function Mago\Internal\extract_archive;
use function Mago\Internal\get_archive_checksum;
use function mkdir;
use function rmdir;
use function sys_get_temp_dir;
use function uniqid;
use function unlink;

/**
 * The wrapper runs the `mago` it downloads from the GitHub release, so it extracts an archive only when the archive's
 * SHA-256 is the one the heyjordanparker/mago-sharp release pins in composer/sha256sums.txt.
 */
final class ExtractArchiveTest extends TestCase
{
    private string $directory = '';

    protected function setUp(): void
    {
        $this->directory = sys_get_temp_dir() . '/' . uniqid('mago-extract-', true);
        mkdir("{$this->directory}/release", 0o755, true);
    }

    protected function tearDown(): void
    {
        $entries = new RecursiveIteratorIterator(
            new RecursiveDirectoryIterator($this->directory, FilesystemIterator::SKIP_DOTS),
            RecursiveIteratorIterator::CHILD_FIRST,
        );
        /** @var SplFileInfo $entry */
        foreach ($entries as $entry) {
            if ($entry->isDir()) {
                rmdir($entry->getPathname());
                continue;
            }

            unlink($entry->getPathname());
        }

        rmdir($this->directory);
    }

    public function testAnArchiveWithTheSha256TheReleasePinsInstalls(): void
    {
        $archive = $this->archive();

        extract_archive($archive, (string) hash_file('sha256', $archive), "{$this->directory}/release", '.tar.gz');

        self::assertSame('the mago binary', file_get_contents("{$this->directory}/release/mago-1.0.0-test/mago"));
        self::assertFileDoesNotExist($archive);
    }

    public function testAnArchiveWithAnotherSha256IsRefusedWithBothHashesAndNothingIsExtracted(): void
    {
        $archive = $this->archive();
        $actual = hash_file('sha256', $archive);
        $pinned = hash('sha256', 'the archive the release built');

        try {
            extract_archive($archive, $pinned, "{$this->directory}/release", '.tar.gz');
            self::fail('A tampered archive was extracted.');
        } catch (RuntimeException $exception) {
            self::assertSame(
                "The downloaded mago-1.0.0-test.tar.gz has SHA-256 {$actual}, and heyjordanparker/mago-sharp pins "
                . "{$pinned} for it, so it is not the archive the release built. It was deleted, and nothing was "
                . 'extracted.',
                $exception->getMessage(),
            );
        }

        self::assertFalse(file_exists("{$this->directory}/release/mago-1.0.0-test"));
        self::assertFileDoesNotExist($archive);
    }

    public function testTheChecksumIsTheOneTheReleaseFileNamesForTheArchive(): void
    {
        $pinned = hash('sha256', 'linux');
        file_put_contents(
            "{$this->directory}/sha256sums.txt",
            hash('sha256', 'darwin')
            . "  mago-1.0.0-aarch64-apple-darwin.tar.gz\n"
            . "{$pinned}  mago-1.0.0-x86_64-unknown-linux-gnu.tar.gz\n",
        );

        self::assertSame($pinned, get_archive_checksum(
            "{$this->directory}/sha256sums.txt",
            'mago-1.0.0-x86_64-unknown-linux-gnu.tar.gz',
        ));
    }

    public function testAnArchiveTheReleaseFileDoesNotNameIsRefused(): void
    {
        file_put_contents(
            "{$this->directory}/sha256sums.txt",
            hash('sha256', 'darwin') . "  mago-1.0.0-aarch64-apple-darwin.tar.gz\n",
        );

        $this->expectExceptionObject(
            new RuntimeException(
                "{$this->directory}/sha256sums.txt pins no SHA-256 for mago-1.0.0-x86_64-unknown-linux-gnu.tar.gz, so "
                . 'its download cannot be verified.',
            ),
        );

        get_archive_checksum("{$this->directory}/sha256sums.txt", 'mago-1.0.0-x86_64-unknown-linux-gnu.tar.gz');
    }

    public function testAPackageWithoutTheReleaseFileIsRefused(): void
    {
        $this->expectExceptionObject(
            new RuntimeException(
                "{$this->directory}/sha256sums.txt does not exist, so no mago download can be verified. Only a "
                . 'heyjordanparker/mago-sharp release ships it: require a release version.',
            ),
        );

        get_archive_checksum("{$this->directory}/sha256sums.txt", 'mago-1.0.0-x86_64-unknown-linux-gnu.tar.gz');
    }

    /**
     * A release archive as release.yml packs it: one `mago-{version}-{triple}` directory holding the binary. gzencode
     * compresses it, since PharData reads back an empty file from an archive its own compress() wrote in the same
     * process.
     */
    private function archive(): string
    {
        $tar = new PharData("{$this->directory}/mago-1.0.0-test.tar");
        $tar->addFromString('mago-1.0.0-test/mago', 'the mago binary');
        unset($tar);
        file_put_contents(
            "{$this->directory}/mago-1.0.0-test.tar.gz",
            gzencode((string) file_get_contents("{$this->directory}/mago-1.0.0-test.tar")),
        );
        unlink("{$this->directory}/mago-1.0.0-test.tar");

        return "{$this->directory}/mago-1.0.0-test.tar.gz";
    }
}
