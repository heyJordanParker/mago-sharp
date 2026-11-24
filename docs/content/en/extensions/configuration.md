+++
title = "Configuring extension hosts"
description = "Configure extension host files, environments, and deadlines."
nav_order = 30
nav_section = "Extensions"
+++
# Configuring extension hosts

Extension hosts are configured under `[extension-hosts]`. Each named entry names one PHP file that registers the host's worker. Mago runs the file inside itself, on the PHP# engine linked into the `mago` binary, so no `php` needs to be installed.

```toml
[extension-hosts.framework]
enabled = true
file = ".mago/framework-worker.php"
environment = { APP_ENV = "analysis" }
ini = { memory_limit = "2G", error_log = "var/log/mago-host.log" }
request-timeout-ms = 30000
```

## Options

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `enabled` | boolean | `true` | Whether Mago runs this host. A disabled host may omit `file`. |
| `file` | path | none | The PHP file that registers the host's worker, as `(new Worker(...))->run()` does. Required when enabled. A relative path resolves from the directory of the configuration file Mago loaded, whether `--config` names it or Mago finds it, for an inherited host too. With no configuration file, it resolves from the workspace. |
| `environment` | string map | `{}` | Variables the worker reads through `getenv()`, `$_ENV` and `$_SERVER`, over Mago's own environment. |
| `ini` | string map | `{}` | PHP settings the worker runs with, as `php -d` sets them, before the host's file runs: `memory_limit`, `error_log` for the file PHP logs to, or any other `php.ini` directive. A relative `error_log` resolves from the directory the interpreter starts in. A setting PHP doesn't know, or a value it refuses, fails the run and names the setting. |
| `request-timeout-ms` | positive integer | `30000` | Longest one unit of a request may run. It counts from when the worker starts a request, and starts again each time the worker starts one of the request's units: a hook, before-analysis, after-file, after-analysis or node, a file's hooks in an after-file batch, one plugin's initialization hooks, a codebase-scan hook, or an extension's worker reducer. So a batch of many files, or many after-analysis hooks, lives while each unit finishes in time. A provider, lint or issue-filter request is one unit. Nested metadata or type-comparison requests don't start it again, so a hook that never finishes fails, and the error names the host and the hook that made no progress, such as the after-analysis hook `SpinsRule` of plugin `rules`. |

## How a host runs

Each analysis thread runs one PHP# interpreter. An interpreter starts in the directory of the configuration file Mago loaded, loads Mago's own copy of the extension SDK, and then runs the file of every enabled host, in the order of their names. The copy of the SDK in the project's `vendor` folder never loads, so a host always speaks the protocol of the `mago` that runs it.

The interpreter ignores `php.ini` and runs without a memory limit, with the `ini` settings of every enabled host over that. All hosts share each interpreter, so two hosts that set one setting set it to the same value, or the run fails naming both. It has the extensions of the PHP# runtime image, so a host loads the classes of the project and its packages as the app does.

A file that fails, or registers no worker, fails the whole run, and the error names the host. A hook that makes no progress for `request-timeout-ms` is interrupted, and its interpreter starts again before its next request.

## Environment

All hosts share each interpreter, so they share its environment too. Two hosts that set one variable must set it to the same value, or Mago refuses to start them and names both hosts and the variable:

```toml
[extension-hosts.routes]
file = ".mago/routes-worker.php"
environment = { APP_ENV = "analysis" }

[extension-hosts.models]
file = ".mago/models-worker.php"
environment = { APP_ENV = "analysis" }
```

A host runs with the permissions of the Mago process.

## Multiple hosts and extensions

Each host has its own file, deadline and environment. One worker may register several logical extensions:

```php
(new Worker(
    LaravelExtension::create(),
    PHPUnitExtension::create(),
))->run();
```

All hosts share each interpreter, and PHP declares a class once, so two hosts that declare one class are an error that names both hosts and the class. Across all enabled hosts, extension identifiers, linter issue codes, and analyzer plugin selectors must remain unique. Extension identifiers and analyzer plugin selectors are compared ASCII case-insensitively; linter rule codes are case-sensitive. External linter codes and analyzer selectors must not collide with their native counterparts.

## Analyzer plugin selection

Analyzer plugins declare whether they are enabled by default. Explicitly enable plugins by identifier or alias under `[analyzer]`:

```toml
[analyzer]
plugins = ["acme/laravel"]
```

When `disable-default-plugins = true`, only explicitly listed plugins run. This setting applies to Mago's built-in analyzer plugins and external analyzer plugins together.

## Layered configuration

Extension hosts participate in normal configuration merging. Host entries merge by their table name, so a project can inherit a host and disable or adjust it:

```toml
extends = "../mago.base.toml"

[extension-hosts.framework]
enabled = false
```

A layer that declares its own `file` gets exactly that file, and a layer that does not keeps the inherited one. `environment` merges by variable name.

See [Configuration](/guide/configuration/) for path resolution and merge precedence.

## Build-time scans without dev dependencies

An app whose own analyzer extension writes a file the app reads at boot runs `mago analyze --scan-only` in its production build. That build usually installs with `composer install --no-dev`, so a host file that loads a `require-dev` package fails there. Mago doesn't skip a host whose file fails: the whole run fails with exit code 2.

Give each extension its own host, so the app's extension never shares a file with a `require-dev` one:

```toml
# mago.toml
[extension-hosts.app]
file = ".mago/app-worker.php"

[extension-hosts.dev]
file = ".mago/dev-worker.php"
```

Beside `mago.toml`, add a scan configuration that extends it and turns the `require-dev` host off:

```toml
# mago.scan.toml
extends = "mago.toml"

[extension-hosts.dev]
enabled = false
```

The production build runs the scan with that file:

```sh
composer install --no-dev
vendor/bin/mago --config mago.scan.toml analyze --scan-only
```

Host entries merge by name, so the scan configuration changes only `enabled`: `dev` keeps its inherited file, and `app` runs as `mago.toml` declares it. Mago never runs a disabled host. `vendor/bin/mago` comes from `heyjordanparker/mago-sharp`, so that package belongs in `require`, not `require-dev`.

Don't use `--no-extensions` for this. It turns every host off, the app's own too, so the scan exits 0 and writes nothing.

### Where paths resolve

Keep `mago.scan.toml` in the same directory as `mago.toml`. Mago resolves every host from the directory of the configuration file it loaded. Under `--config` that is the scan configuration's directory, for the hosts it inherits too:

- Each host's `file`, such as `.mago/app-worker.php`, resolves from that directory.
- Each interpreter starts in that directory, so a host reads a relative path from there.

`[source] paths` resolve against the workspace instead, which is the current directory unless `--workspace` or `[source] workspace` names another. Run the scan from the project root.

A scan configuration can't narrow what Mago scans. Arrays concatenate through `extends`, so its `[source] paths` add to the paths of `mago.toml`.

### What the scan runs

`--scan-only` builds the codebase and analyzes no file. A file that doesn't parse still fails the scan: its parse error is an error in the report, `--reporting-format json` included, and the scan exits non-zero. It runs the hooks of every enabled host in this order:

1. initialization hooks, where `addStub()` declares synthetic classes
2. codebase-scan hooks
3. before-analysis hooks

After-file and after-analysis hooks don't run. When `[source] paths` match no file, Mago warns `No files found to analyze.` and exits 0 after the initialization hooks. The codebase-scan and before-analysis hooks never run, so an extension that writes its file there writes nothing.
