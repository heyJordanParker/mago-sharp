+++
title = "Upgrading"
description = "Keep Mago up to date with the self-update command, including pinning to a specific version or syncing with the version field in mago.toml."
nav_order = 80
nav_section = "Guide"
+++
# Upgrading

`mago self-update` replaces the running binary with a newer release. Use it for installs that came from the shell script or a manual download.

> Composer installs are different. The Composer wrapper pins a binary that matches the Composer package version, so you upgrade Mago with `composer update` rather than `self-update`.

## Verification

`self-update` verifies the archive it downloads before it extracts anything from it, and refuses one that fails, leaving the installed binary as it was:

- With the [GitHub CLI](https://cli.github.com/) on your `PATH`, it checks the archive's build attestation with `gh attestation verify`, which proves the release workflow built it.
- Without it, it checks the archive's SHA-256 against the release's `mago-<version>-sha256sums.txt`, and warns that full provenance needs `gh`: a checksum from the release cannot tell a replaced release apart. An older release that publishes no sums file installs only with `gh`.

## Common flows

Check for updates without installing:

```sh
mago self-update --check
```

The command prints the new version (if any) and exits non-zero when one is available, which makes it scriptable in CI.

Update to the latest release:

```sh
mago self-update                  # interactive confirmation
mago self-update --no-confirm     # skip the prompt
```

Pin a specific version:

```sh
mago self-update --tag 0.6.1
```

## Sync with the project's version pin

If your `mago.toml` uses [version pinning](/guide/configuration/#version-pinning), you can sync the installed binary to whatever the project expects without typing the version yourself:

```sh
mago self-update --to-project-version
```

For an exact pin (`version = "0.6.1"`), this resolves directly to that release tag. For a major or minor pin, Mago scans recent GitHub releases and installs the highest one that still satisfies the pin. So `version = "0"` with 1.0 already shipped still installs the latest 0.x release. `version = "0.2"` with 0.6.x in the wild walks back to the latest 0.2.x.

The command fails only if no published release satisfies the pin at all.

## Reference

```sh
Usage: mago self-update [OPTIONS]
```

| Flag | Description |
| :--- | :--- |
| `--check`, `-c` | Check for updates without installing. Exits non-zero when an update is available. |
| `--no-confirm` | Skip the interactive confirmation prompt. |
| `--tag <VERSION>` | Install a specific release tag instead of the latest. Mutually exclusive with `--to-project-version`. |
| `--to-project-version` | Install whatever the project's `version` pin demands. Fails if no pin is set. Mutually exclusive with `--tag`. |
| `-h`, `--help` | Print help and exit. |

Global flags must come before `self-update`. See the [CLI overview](/fundamentals/command-line-interface/) for the full list.
