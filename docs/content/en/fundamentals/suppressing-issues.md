+++
title = "Suppressing issues"
description = "How a project turns a rule off, for every file or for the files it names, in mago.toml."
nav_order = 30
nav_section = "Fundamentals"
+++
# Suppressing issues

Fixing the underlying problem is almost always the right answer. Sometimes it isn't: legacy code, false positives, deliberate exceptions. For those cases, a project turns the rule off in its `mago.toml`, for every file or for the files it names.

A comment is text in every file Mago reads. A comment that names an issue code, such as `// @mago-ignore lint:no-shorthand-ternary` or `/** @mago-expect analysis:mixed-operand */`, hides nothing, so every exception the project makes sits in one file, where a review sees it.

## Linter rules

Each rule under `[linter.rules]` takes `enabled` and `exclude`. `enabled = false` turns the rule off for the project, and `exclude` turns it off for the paths or globs it lists while every other rule still applies to them.

```toml
[linter.rules]
no-shorthand-ternary = { enabled = false }
no-eval = { exclude = ["src/Templating/Compiler.php"] }
cyclomatic-complexity = { threshold = 6, exclude = ["src/Legacy/", "**/*Test.php"] }
```

The [linter configuration reference](/tools/linter/configuration-reference/) lists every option.

## Analyzer issues

The analyzer's `ignore` list hides the codes it names, everywhere, in the paths an entry's `in` names, or where an entry's `pattern` matches the issue's text.

```toml
[analyzer]
ignore = [
    "mixed-argument",
    { code = "missing-return-type", in = "tests/" },
    { code = "unknown-class-instantiation", in = ["src/Hydrator.php"] },
    { pattern = "Symfony", code = "mixed-assignment" },
]
```

The [analyzer configuration reference](/tools/analyzer/configuration-reference/) describes each form. `mago analyze --skip-ignores` shows what the list hides for one run.

An `ignore` entry can't hide one of the language's errors in a PHP# file: the error stays reported, with an `unsuppressible-error` warning on it. It hides an error of an extension host's rule, whose code names its host, as `acme/no-raw-sql`, as it hides any other issue.

## Files a tool skips

`excludes` under `[linter]`, `[analyzer]` or `[guard]` removes files from that one tool. `[source].excludes` removes them from every tool.

```toml
[guard]
excludes = ["src/ThirdParty/**"]
```

## Existing issues

A [baseline](/fundamentals/baseline/) records the issues a project has today, so a run reports only new ones while the old ones are fixed.
