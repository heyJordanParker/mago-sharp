#!/usr/bin/env php
<?php

declare(strict_types=1);

namespace Mago\Scripts;

use RuntimeException;

use function count;
use function dirname;
use function explode;
use function file_get_contents;
use function file_put_contents;
use function fwrite;
use function implode;
use function is_file;
use function is_string;
use function preg_match;
use function preg_quote;
use function preg_replace;
use function rtrim;
use function sprintf;
use function str_replace;
use function substr_count;
use function trim;

use const STDERR;
use const STDOUT;

const VERSIONED_DOCUMENTATION_FILES = [
    // PHP#'s README, whose runtime image examples name the release's tags.
    '../README.md',
    'README.md',
    'docs/content/en/extensions/development/packaging-and-compatibility.md',
    'docs/content/en/extensions/getting-started.md',
    'docs/content/en/guide/configuration.md',
    'docs/content/en/guide/installation.md',
    'docs/content/en/guide/upgrading.md',
    'docs/content/en/recipes/github-actions.md',
    'docs/content/fr/guide/configuration.md',
    'docs/content/fr/guide/installation.md',
    'docs/content/fr/guide/upgrading.md',
    'docs/content/fr/recipes/github-actions.md',
    'docs/content/zh/guide/configuration.md',
    'docs/content/zh/guide/installation.md',
    'docs/content/zh/guide/upgrading.md',
    'docs/content/zh/recipes/github-actions.md',
    // The tag example in a comment, the pin examples of `mago self-update --help`, and those of the configuration schema.
    'scripts/install.sh',
    'src/commands/self_update.rs',
    'src/config/mod.rs',
];

function set_version(mixed $newVersion, int $argumentCount): int
{
    $newVersion = namespace\validate_new_version($newVersion, $argumentCount);
    if (null === $newVersion) {
        return 64;
    }

    $root = dirname(__DIR__);
    $manifestPath = $root . '/Cargo.toml';
    $manifest = namespace\read_file($manifestPath);
    $currentVersion = namespace\read_package_version($manifest);

    if (!namespace\versions_share_major($currentVersion, $newVersion)) {
        fwrite(STDERR, sprintf(
            "Major version bumps are not supported by this script: %s to %s.\n",
            $currentVersion,
            $newVersion,
        ));

        return 64;
    }

    if ($currentVersion === $newVersion) {
        fwrite(STDOUT, sprintf("Mago is already at version %s.\n", $newVersion));

        return 0;
    }

    $updates = namespace\prepare_updates($root, $manifest, $currentVersion, $newVersion);
    namespace\write_updates($updates);

    fwrite(STDOUT, sprintf("Updated Mago from %s to %s in %d files.\n", $currentVersion, $newVersion, count($updates)));

    return 0;
}

function validate_new_version(mixed $newVersion, int $argumentCount): ?string
{
    if (1 !== $argumentCount || !is_string($newVersion)) {
        fwrite(STDERR, "Usage: php scripts/set-version.php <version>\n");

        return null;
    }

    if (1 !== preg_match('/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/D', $newVersion)) {
        fwrite(STDERR, sprintf("Invalid release version: %s\n", $newVersion));

        return null;
    }

    return $newVersion;
}

function versions_share_major(string $currentVersion, string $newVersion): bool
{
    [$currentMajor] = explode('.', $currentVersion);
    [$newMajor] = explode('.', $newVersion);

    return $currentMajor === $newMajor;
}

function minor_version(string $version): string
{
    [$major, $minor] = explode('.', $version);

    return $major . '.' . $minor;
}

/**
 * The release version is the root package's alone: the `mago` binary, PHP#'s engine and the Composer packages take it
 * from there. Every workspace crate keeps version 0.0.0, which a release never bumps, so a bump rebuilds only what
 * reads the release version.
 *
 * @return array<string, string>
 */
function prepare_updates(string $root, string $manifest, string $currentVersion, string $newVersion): array
{
    $lockPath = $root . '/Cargo.lock';
    $updates = [
        $root . '/Cargo.toml' => namespace\update_manifest($manifest, $newVersion),
        $lockPath => namespace\update_lock($lockPath, namespace\read_file($lockPath), $currentVersion, $newVersion),
    ];

    return $updates + namespace\prepare_documentation_updates($root, $currentVersion, $newVersion);
}

/** @return array<string, string> */
function prepare_documentation_updates(string $root, string $currentVersion, string $newVersion): array
{
    $updates = [];

    foreach (VERSIONED_DOCUMENTATION_FILES as $relativePath) {
        $path = $root . '/' . $relativePath;
        $contents = namespace\read_file($path);
        $updated = namespace\update_documentation($path, $contents, $currentVersion, $newVersion);

        if ($updated === $contents) {
            continue;
        }

        $updates[$path] = $updated;
    }

    return $updates;
}

function update_documentation(string $path, string $contents, string $currentVersion, string $newVersion): string
{
    $updated = str_replace($currentVersion, $newVersion, $contents);
    $currentMinorVersion = namespace\minor_version($currentVersion);
    $newMinorVersion = namespace\minor_version($newVersion);

    if ($currentMinorVersion === $newMinorVersion) {
        return $updated;
    }

    $pattern = sprintf('/(?<![0-9.])%s(?![0-9]|\.[0-9])/', preg_quote($currentMinorVersion, '/'));
    $updated = preg_replace($pattern, $newMinorVersion, $updated);
    if (null === $updated) {
        throw new RuntimeException(sprintf('Unable to update minor versions in %s.', $path));
    }

    return $updated;
}

/** @param array<string, string> $updates */
function write_updates(array $updates): void
{
    foreach ($updates as $path => $contents) {
        if (false === file_put_contents($path, $contents)) {
            throw new RuntimeException(sprintf('Unable to write %s.', $path));
        }
    }
}

function read_file(string $path): string
{
    if (!is_file($path)) {
        throw new RuntimeException(sprintf('Required file does not exist: %s.', $path));
    }

    $contents = file_get_contents($path);
    if (false === $contents) {
        throw new RuntimeException(sprintf('Unable to read %s.', $path));
    }

    return $contents;
}

function read_package_version(string $manifest): string
{
    $section = null;
    foreach (explode("\n", $manifest) as $line) {
        $line = rtrim($line, "\r");
        $matches = [];
        if (1 === preg_match('/^\[([^]]+)]$/', trim($line), $matches)) {
            $section = $matches[1] ?? throw new RuntimeException('Unable to read a Cargo.toml section name.');

            continue;
        }

        $matches = [];
        if ('package' === $section && 1 === preg_match('/^version = "([^"]+)"$/', $line, $matches)) {
            return $matches[1] ?? throw new RuntimeException('Unable to read the root package version.');
        }
    }

    throw new RuntimeException('Unable to find the root package version in Cargo.toml.');
}

function update_manifest(string $manifest, string $newVersion): string
{
    $section = null;
    $updated = false;
    $lines = explode("\n", $manifest);

    foreach ($lines as &$line) {
        $normalized = rtrim($line, "\r");
        $nextSection = namespace\read_manifest_section($normalized);
        if (null !== $nextSection) {
            $section = $nextSection;

            continue;
        }

        if (!$updated && 'package' === $section && 1 === preg_match('/^version = "[^"]+"$/', $normalized)) {
            $line = namespace\update_manifest_line($line, $newVersion);
            $updated = true;
        }
    }
    unset($line);

    if (!$updated) {
        throw new RuntimeException('Unable to find the root package version in Cargo.toml.');
    }

    return implode("\n", $lines);
}

function read_manifest_section(string $line): ?string
{
    $matches = [];
    if (1 !== preg_match('/^\[([^]]+)]$/', trim($line), $matches)) {
        return null;
    }

    return $matches[1] ?? throw new RuntimeException('Unable to read a Cargo.toml section name.');
}

function update_manifest_line(string $line, string $newVersion): string
{
    $updated = preg_replace('/version = "[^"]+"/', sprintf('version = "%s"', $newVersion), $line, 1);
    if (null === $updated) {
        throw new RuntimeException('Unable to update a version in Cargo.toml.');
    }

    return $updated;
}

/**
 * Moves the root package, mago, from the current version to the new one.
 */
function update_lock(string $path, string $lock, string $currentVersion, string $newVersion): string
{
    $current = sprintf("\nname = \"mago\"\nversion = \"%s\"\n", $currentVersion);
    if (1 !== substr_count($lock, $current)) {
        throw new RuntimeException(sprintf('%s holds no mago package at version %s.', $path, $currentVersion));
    }

    return str_replace($current, sprintf("\nname = \"mago\"\nversion = \"%s\"\n", $newVersion), $lock);
}

try {
    exit(namespace\set_version($argv[1] ?? null, count($argv) - 1));
} catch (RuntimeException $exception) {
    fwrite(STDERR, $exception->getMessage() . "\n");

    exit(1);
}
