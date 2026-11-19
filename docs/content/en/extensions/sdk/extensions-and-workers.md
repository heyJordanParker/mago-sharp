+++
title = "Extensions and workers"
description = "Define logical extensions and run them in the PHP workers Mago runs."
nav_order = 10
nav_section = "Extensions"
nav_subsection = "PHP SDK"
+++
# Extensions and workers

`Mago\Sdk\Extension` describes one logical package. `Mago\Sdk\Worker` registers one or more of those packages and serves Mago requests until shutdown.

## Defining an extension

An extension package should expose a factory that owns its complete registration:

```php
namespace Vendor\Framework\Mago;

use Vendor\Framework\Mago\Analyzer\FrameworkAnalyzerPlugin;
use Vendor\Framework\Mago\Linter\Rules\FrameworkRule;
use Vendor\Framework\Mago\Worker\MetricsReducer;
use Mago\Sdk\Extension;

final class FrameworkExtension
{
    private function __construct() {}

    public static function create(bool $strict = false): Extension
    {
        $metrics = new MetricsReducer();

        return new Extension(
            identifier: 'vendor/framework',
            name: 'Framework support',
            version: '2.3.0',
            linterRules: [new FrameworkRule($strict)],
            analyzerPlugins: [new FrameworkAnalyzerPlugin($metrics)],
            workerReducer: $metrics,
        );
    }
}
```

Keep the identifier, version, default capability set, and shared object wiring inside the package. Factory parameters expose only intentional typed options. A consuming project should not instantiate `Extension` for a third-party package or manually choose which of its rules and plugins to register.

| Argument | Contract | Purpose |
| :--- | :--- | :--- |
| `identifier` | non-empty string | Stable, globally unique extension identifier. Use a vendor-qualified value. |
| `name` | non-empty string | Human-readable display name. |
| `version` | non-empty string | Extension package version reported during registration. |
| `linterRules` | `list<Rule>` | Custom linter rules exposed by the extension. |
| `analyzerPlugins` | `list<Plugin>` | Analyzer plugins exposed by the extension. |
| `workerReducer` | `?WorkerReducer` | Optional terminal aggregation for interpreter-local state. |

Extension identifiers and analyzer plugin selectors are compared ASCII case-insensitively. Linter rule codes are case-sensitive and must be unique within an extension, across every enabled host, and against Mago's native rule codes. Analyzer plugin identifiers and aliases must not collide with another external or native plugin selector.

## Running a worker

```php
use Vendor\Framework\Mago\FrameworkExtension;
use Mago\Sdk\Worker;

(new Worker(
    FrameworkExtension::create(strict: true),
))->run();
```

`run()` owns the process protocol loop. It normally reads from the SDK-selected input transport and writes frames to standard output. The optional input/output resource arguments exist for embedding and tests; production entrypoints should use the defaults.

Do not write application output to standard output. Use standard error for debugging:

```php
fwrite(STDERR, "Framework extension initialized.\n");
```

The worker captures PHP output-buffer output and redirects throwable details to standard error. Direct writes to `STDOUT`, output emitted before `run()` installs its buffer, and native output that bypasses PHP's output buffer can still corrupt protocol frames.

## Hosting several extensions

The constructor is variadic:

```php
(new Worker(
    LaravelExtension::create(),
    PHPUnitExtension::create(),
    CompanyRulesExtension::create(),
))->run();
```

This shares one worker among the extensions. It is useful when packages are always deployed together. Use separate configured hosts when extensions require different environments or timeouts.

## Interpreter-local state

Rules, plugins, providers, and reducers are constructed once per interpreter, as each one runs the host's file. Their object properties persist across requests handled by that interpreter, until the Mago command finishes.

This state is:

- local to one interpreter, not shared with the other analysis threads' interpreters;
- retained across files and incremental-analysis generations while the interpreter lives;
- lost when Mago starts the interpreter again, after a deadline or when a class it loaded changed;
- not guaranteed to receive requests in file order.

An interpreter runs one request at a time. A hook that waits on a codebase query keeps its turn, so no other hook in that interpreter starts until it returns. A rule can fill a cache on its own object around a query, and every later file in that interpreter reads the full result. Mago gets parallelism by running one interpreter per analysis thread.

Mago counts what any hook or provider reads from the codebase, such as a class from `Codebase::getClassLike()` or the methods `Codebase::findMethods()` finds across classes, as read by every file, as [reads and incremental analysis](/extensions/analyzer/codebase-metadata/#reads-and-incremental-analysis) says. So a rule may keep what it read on its own object and answer later files from it: every file is analyzed again when what was read changes, as a class's signature or a search's result, and none is when it stays the same. A before-analysis hook runs in one interpreter, so what it keeps stays there, and a provider in another interpreter never sees it. A provider that needs codebase facts reads them when it is asked, as through `findMethods()` inside `getPropertyType()`, and the codebase answers the same read again from its cache for the rest of the analysis in that interpreter.

Key an analysis cache on a plugin's object by `Codebase::generation()`, and fill it again when the generation changes, since the object outlives the analysis that filled it. Use a `WorkerReducer` when interpreter-local application metrics must be combined at shutdown.

## PHP version

Mago sends its configured target PHP version during registration. It is exposed as `Mago\Sdk\PHPVersion` through `SourceFile` and analyzer contexts.

```php
if ($context->phpVersion->isAtLeast(PHPVersion::fromParts(8, 4))) {
    // Apply PHP 8.4-specific behavior.
}
```

The numeric identifier uses `0xMMMMmmpp`: a 16-bit major version followed by 8-bit minor and patch components. Prefer `fromParts()`, `major()`, `minor()`, `patch()`, and `isAtLeast()` over inspecting `id` directly.

## Deadlines

A unit of a request that runs past the host's `request-timeout-ms` is interrupted at its next PHP instruction, and the check fails with an error naming the host and the unit. The interpreter starts again before its next request. A callback needs no check of its own, so a long loop needs no code to stop at the deadline.

## Exceptions

A callback exception becomes a protocol error with callback identity and the original message. Optional analyzer provider failures fall back to native analysis, while registration and lifecycle failures fail the operation.
