<?php

declare(strict_types=1);

namespace Mago\Internal;

use Composer\InstalledVersions;
use RuntimeException;

use function ltrim;

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
 * The error for a `mago` and an extension SDK of different releases, as "`mago` speaks analyzer protocol 1.12, and the
 * extension SDK, heyjordanparker/mago-sharp 1.2.3, speaks 1.13.": what each side holds, this SDK's release when
 * Composer gives one, and the fix, since `mago` and the SDK release together at one version.
 *
 * @internal
 */
function release_mismatch(string $mago, string $sdk): string
{
    $package = 'heyjordanparker/mago-sharp';
    try {
        $named = "{$package} " . namespace\get_version();
    } catch (RuntimeException) {
        $named = $package;
    }

    return (
        "`mago` {$mago}, and the extension SDK, {$named}, {$sdk}. `mago` and {$package} must be the same release: "
        . "run the `mago` that {$package} installs, `vendor/bin/mago`, or require the {$package} release "
        . '`mago --version` prints.'
    );
}
