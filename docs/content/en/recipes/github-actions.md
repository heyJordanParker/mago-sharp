+++
title = "GitHub Actions recipe"
description = "Run formatting, linting, and analysis on every push and pull request."
nav_order = 50
nav_section = "Recipes"
+++
# GitHub Actions recipe

A simple workflow that runs the formatter, linter, and analyzer on every push and pull request, with native PR annotations.

## Quick setup

The workflow installs Mago through Composer, so add it to your project first:

```sh
composer require --dev "heyjordanparker/mago-sharp:^0.3.1"
```

Then create `.github/workflows/mago.yml`:

```yaml
name: Mago Code Quality

on:
  push:
  pull_request:

jobs:
  mago:
    name: Run Mago Checks
    runs-on: ubuntu-latest
    env:
      GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
    steps:
      - name: Checkout
        uses: actions/checkout@v6

      - name: Set up PHP
        uses: shivammathur/setup-php@v2
        with:
          php-version: '8.4'

      - name: Install dependencies
        run: composer install --no-interaction --no-progress

      - name: Check formatting
        run: vendor/bin/mago format --check

      - name: Lint
        if: success() || failure()
        run: vendor/bin/mago lint

      - name: Analyze
        if: success() || failure()
        run: vendor/bin/mago analyze
```

A few notes on the structure:

- Set `php-version` to the PHP version your project runs on. `composer install` also gives the analyzer your dependencies, which it needs to resolve symbols.
- The first call to `vendor/bin/mago` downloads the matching binary from the GitHub release. `GITHUB_TOKEN` is not exported to steps automatically, so the job passes it explicitly to avoid GitHub's anonymous rate limit on shared runners.
- Splitting `format`, `lint`, and `analyze` into separate steps surfaces findings from all three, even when an earlier step fails. A single combined `run:` would short-circuit on the first failure and hide the rest.
- `if: success() || failure()` runs the step when the job has not been cancelled, which is what you want here. `always()` would also run it after setup failures.
- Use `mago format --check`, not `--dry-run`. `--check` exits non-zero when files need formatting; `--dry-run` only prints a diff and always exits zero.
- Mago detects GitHub Actions through the `GITHUB_ACTIONS` environment variable and switches to `--reporting-format=github` automatically, producing native PR annotations. No extra configuration needed.
