+++
title = "Testing and debugging"
description = "Test extensions against a corpus, validate worker registration, and diagnose protocol failures."
nav_order = 20
nav_section = "Extensions"
nav_subsection = "Development"
+++
# Testing and debugging

Test an extension primarily through a small PHP corpus analyzed by the real Mago CLI. This exercises worker startup, registration, target matching, binary encoding, callbacks, and the resulting diagnostics together.

## Create a corpus

Keep a representative project inside the extension package:

```text
tests/corpus/
├── mago.toml
├── worker.php
└── src/
    ├── analyzer.php
    └── linter.php
```

The worker must load the package under test, not a copied implementation:

```php
<?php

declare(strict_types=1);

use Acme\Mago\AcmeExtension;
use Mago\Sdk\Worker;

require dirname(__DIR__, 2) . '/vendor/autoload.php';

(new Worker(
    AcmeExtension::create(),
))->run();
```

Configure that worker and restrict discovery to the corpus source:

```toml
[source]
paths = ["src"]

[linter]
baseline = "lint-baseline.toml"
baseline-variant = "strict"

[analyzer]
baseline = "analysis-baseline.toml"
baseline-variant = "strict"

[extension-hosts.acme]
file = "worker.php"
```

Split the corpus into focused files that cover:

- matching and non-matching linter targets;
- analyzer behavior with and without a provider result;
- inferred types, argument validation, references, and entry points;
- invalid or incomplete framework metadata;
- interactions between multiple rules or providers.

## Lock the issue baseline

For a representative corpus, generate and commit separate linter and analyzer baseline files:

```sh
mago --workspace tests/corpus lint --generate-baseline
mago --workspace tests/corpus analyze --generate-baseline
```

The strict variant records issue codes and exact line ranges. CI should verify that the current diagnostics still match those files exactly:

```sh
mago --workspace tests/corpus lint --verify-baseline
mago --workspace tests/corpus analyze --verify-baseline
```

Verification fails when a diagnostic is added or removed. Regenerate a baseline only when an intentional extension change alters the expected result, then review the baseline diff before committing it. A comment in a fixture hides no issue, so the baseline is where a corpus states the diagnostics it expects.

Both commands should report zero issues. Treat any non-zero count as a corpus regression.

For behavior whose success is silence, make the fixture fail observably when the extension is wrong. For example, pass a provider's inferred return value to a narrowly typed function so an incorrect type produces a native analyzer issue. Include a negative case where the provider returns `null` and Mago preserves native behavior.

For return/signature pairs, verify both argument validation and the final return type. For references and entry points, verify that unused-symbol diagnostics change only for the intended declaration.

## Unit test package logic separately

Unit tests remain useful for deterministic logic such as framework indexes, configuration validation, metadata mapping, and reducer aggregation. Use the test framework preferred by the package.

Do not replace corpus coverage with parser or type-comparison test doubles. Mago owns those semantics, so only a real CLI run proves that the extension integrates with them correctly.

## Validate registration

Before debugging callbacks, verify the host can start and advertise a stable registration:

```sh
mago extension validate
mago extension list
mago extension list --json
```

`extension validate` exercises host startup and linter registration. `extension list` reports hosts, logical extensions, and linter rules; it does not currently list analyzer plugins. Run an analyzer fixture to validate analyzer registration, plugin selection, targets, and capabilities.

Every interpreter runs the host's file, so its worker must register identical extension identifiers, rule definitions, plugin selectors, targets, and capabilities in each. Do not make registration depend on a random value, request order, or mutable remote service.

## Inspect failures

Enable trace output:

```sh
MAGO_LOG=trace mago analyze --reporting-format count
```

A worker that fails or throws names its host in the error, with what PHP reported, as ``extension host `reports` failed: PHP Fatal error: ...``. Log concise context to `STDERR`; output a worker prints never reaches Mago's report.

Useful isolation switches are:

- `--no-extensions` to confirm the failure is extension-related;
- a disabled host in a configuration overlay;
- `[analyzer].disable-default-plugins` plus an explicit plugin selector;
- `lint --only vendor/rule` for one external rule;
- `--threads 1` to make interpreter-local behavior reproducible.

## Timeouts

The host's `request-timeout-ms` is the longest one unit of a request may run. It counts from when the worker starts a request, and starts again each time the worker starts one of the request's units: a file's hooks in an after-file batch, one plugin's initialization, before-analysis or after-analysis hooks, a codebase-scan hook, or an extension's worker reducer. It does not start again at the nested metadata or comparison requests a hook makes. When a hook runs past it, Mago interrupts it, and the error names the unit that made no progress, such as the after-analysis hooks of plugin `dead-routes`. The interpreter starts again before its next request. Mago interrupts the hook at its next PHP instruction, so a long scan or aggregation loop needs no check of its own.

## Test failure policy

Provider failures should leave native analysis usable. Test that a throwing or disconnected optional provider falls back rather than inventing a type. Lifecycle failures should fail the analysis operation with host or plugin context; they are not ordinary source diagnostics.

Also test malformed framework data. An extension should return `null` or report a precise issue rather than sending invalid SDK values that turn a user-code problem into a protocol error.

## Cross-platform checks

Run integration tests on Linux and macOS. Mago runs each host's file on the engine linked into its own binary, so a host runs the same on every machine with no `php` installed. Extension entrypoints should call `Worker::run()` normally.

## Incremental and repeated analysis

Workers can serve repeated analysis generations during one command. Tests should ensure that:

- codebase-scan state is cleared on `firstBatch`;
- provider caches do not leak stale project metadata across generations;
- after-file references are replaced, not duplicated;
- reducer state represents one intended worker lifetime;
- exact in-memory source is used instead of rereading changed files from disk.

## Read source through the SDK

A host reads a file's source through the SDK, never from disk. The file a hook sees may exist only in memory: `mago convert` proves each `.sharp` file before it writes it, and `--dry-run` never writes it, so a hook that runs `file_get_contents()` on `app/Billing/Invoice.sharp` during that proof fails. The SDK gives every hook that inspects a file that file's `SourceFile`, whose `contents` are the exact bytes Mago analyzes:

- an after-file hook calls `FileAnalysis::getSourceFile()`
- a node-analysis hook reads `NodeAnalysisContext::$source`
- a lint rule reads `LintContext::$file`

```php
$source = $analysis->getSourceFile();
$lines = explode("\n", $source->contents);
```

`SourceFile::$path` names the file for an issue. Never open it.
