+++
title = "Extensions"
description = "Extend Mago with external linter rules and analyzer plugins."
nav_order = 10
nav_section = "Extensions"
+++
# Extensions

Mago extensions add project- and framework-specific behavior without rebuilding Mago. A configured extension host is an external program connected through Mago's language-neutral worker protocol. Each host worker advertises one or more logical extensions, and the host can be written in any language capable of implementing that protocol.

Mago bundles a first-party PHP SDK with `heyjordanparker/mago-sharp`. The examples and API reference in this manual use that SDK, but the host architecture does not require PHP.

An extension may contribute:

- **Linter rules** that inspect selected concrete-syntax-tree nodes and report issues or suggested edits.
- **Analyzer plugins** that provide types, callable signatures, flow assertions, framework entry points, initialization knowledge, codebase scans, and analysis hooks.
- **Worker reduction** that merges interpreter-local data once Mago stops the hosts.

The formatter and guard do not currently expose extension APIs.

## When to write an extension

An extension is appropriate when knowledge belongs to a framework, library, or application rather than PHP itself. Typical examples include:

- inferring the return type of a service-container lookup;
- teaching Mago that framework lifecycle methods initialize properties;
- marking controller actions or test methods as externally referenced;
- reporting a project-specific syntax convention;
- inspecting the final project analysis to calculate type-coverage metrics.

Fix generally applicable PHP behavior in Mago itself. Use an extension for behavior that depends on conventions or runtime wiring Mago cannot infer from PHP source alone.

## Execution and trust

Extension workers run inside Mago, not in a sandbox. They run with the operating-system permissions of the Mago process, in the configuration file's directory, with the environment configured for their host. Install and enable only extensions you trust.

Mago runs every worker on the PHP# engine linked into the `mago` binary, and exchanges binary messages with it. The `mago` binary carries its own copy of the PHP SDK, which it loads before any host, so a worker always speaks the protocol of the `mago` that runs it.

PHP extension authors should use the bundled SDK, which handles registration, source snapshots, analyzer metadata requests, and responses.

Each analysis thread runs one interpreter, which runs every configured extension host's file. Each worker may expose one or more logical extensions. The interpreters give Mago its parallelism, and each serves one request at a time. The PHP SDK represents these concepts with `Worker` and `Extension`.

## Where to begin

- [Getting started](/extensions/getting-started/) builds a complete linter extension.
- [Architecture and execution model](/extensions/architecture/) explains worker pools and lifecycle ordering.
- [Linter extensions](/extensions/linter/overview/) covers syntax-driven rules.
- [Analyzer plugins](/extensions/analyzer/overview/) maps the analyzer extension surface.
- [PHP SDK API index](/extensions/reference/sdk-index/) lists every public SDK type by namespace.
