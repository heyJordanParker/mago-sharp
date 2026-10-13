+++
title = "FAQ"
description = "Common questions about Mago, the project, and what does and does not belong in it."
nav_order = 10
nav_section = "Reference"
+++
# FAQ

## Why the name "Mago"?

The name comes from upstream Mago, which was first named "fennec", after the fennec fox native to North Africa. A name conflict with another tool forced a rename.

Carthage Software, upstream's maintainer, named it after Mago of Carthage, an ancient Carthaginian writer known as the "Father of Agriculture". As he cultivated the land, the tool aims to help developers cultivate their codebases.

The name has a useful double meaning. In Spanish and Italian, "mago" means "magician" or "wizard". The logo, drawn for upstream Mago, captures both: a fennec fox in a wizard's hat and robe, with the ancient Carthaginian symbol of Tanit on its garments.

## How do you pronounce Mago?

`/ˈmɑːɡoʊ/`, "mah-go". Two syllables: "ma" as in "mama", "go" as in "go".

## Does mago-sharp ship a language server?

No. mago-sharp has no Language Server Protocol implementation. Run it from the command line or in CI. [Configuration](/guide/configuration/) covers the JSON schema editors use to validate `mago.toml`, and the terminal links that open a reported file in your editor.

## Does mago-sharp offer editor extensions (VS Code, etc.)?

No. mago-sharp ships no editor-specific extensions.

## Does mago-sharp support analyzer plugins?

Yes, through extensions. An extension is an external program that mago-sharp starts from `[extension-hosts]` in `mago.toml` and talks to over a binary worker protocol. It can add linter rules and analyzer plugins. The formatter and guard have no extension API. mago-sharp ships a PHP SDK for writing extensions: the `Mago\Sdk` namespace in the `heyjordanparker/mago-sharp` Composer package. [Extensions](/extensions/overview/) covers the protocol, the SDK, and a complete example.

## Which tools does mago-sharp include?

One binary runs:

- `mago lint`, the linter.
- `mago analyze`, the static analyzer, for PHP and PHP#.
- `mago format`, the formatter.
- `mago guard`, which enforces architectural layer rules.
- `mago compile`, which compiles PHP# files for the PHP# engine.

`mago fix` applies fixes from the guard, analyzer, linter, and formatter until none of them changes anything.

## Will Mago implement a Composer alternative?

No. Composer is a fantastic tool, and most of its work is I/O-bound. A Rust rewrite would not gain much speed, would fragment the ecosystem, and would make it very difficult to support Composer's PHP-based plugin architecture.

## Will Mago implement a PHP runtime?

No. The PHP runtime is enormous. Even very large efforts (Facebook's HHVM, VK's KPHP) struggled to reach full parity with the Zend Engine. A smaller project cannot do better, and the result would only fragment the community. Mago focuses on tooling, not on runtimes.
