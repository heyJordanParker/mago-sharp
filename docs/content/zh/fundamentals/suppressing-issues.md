+++
title = "抑制问题"
description = "项目如何在 mago.toml 中关闭一条规则,对所有文件或对它列出的文件。"
nav_order = 30
nav_section = "基础"
+++
# 抑制问题

修复底层问题几乎总是正确的答案。但有时并非如此:遗留代码、误报、有意为之的例外。针对这些情况,项目在自己的 `mago.toml` 中关闭该规则,对所有文件或对它列出的文件。

在 Mago 读取的每个文件中,注释都只是文本。写有问题代码的注释,例如 `// @mago-ignore lint:no-shorthand-ternary` 或 `/** @mago-expect analysis:mixed-operand */`,不会隐藏任何问题,因此项目的每个例外都集中在一个文件里,评审时一眼可见。

## Linter 规则

`[linter.rules]` 下的每条规则都接受 `enabled` 和 `exclude`。`enabled = false` 为整个项目关闭该规则,`exclude` 为它列出的路径或 glob 关闭该规则,其他规则仍然作用于这些文件。

```toml
[linter.rules]
no-shorthand-ternary = { enabled = false }
no-eval = { exclude = ["src/Templating/Compiler.php"] }
cyclomatic-complexity = { threshold = 6, exclude = ["src/Legacy/", "**/*Test.php"] }
```

[Linter 配置参考](/tools/linter/configuration-reference/)列出了所有选项。

## 分析器问题

分析器的 `ignore` 列表会隐藏它列出的代码:可以作用于所有地方,也可以只作用于某个条目的 `in` 所列的路径,或者某个条目的 `pattern` 与问题文本匹配的地方。

```toml
[analyzer]
ignore = [
    "mixed-argument",
    { code = "missing-return-type", in = "tests/" },
    { code = "unknown-class-instantiation", in = ["src/Hydrator.php"] },
    { pattern = "Symfony", code = "mixed-assignment" },
]
```

[分析器配置参考](/tools/analyzer/configuration-reference/)描述了每种形式。`mago analyze --skip-ignores` 会在单次运行中显示该列表隐藏的内容。

`ignore` 条目无法隐藏 PHP# 文件中的语言错误:该错误仍会被报告,并附带一条 `unsuppressible-error` 警告。它会像隐藏其他问题一样,隐藏扩展宿主规则的错误,这类错误的代码会带上宿主名,例如 `acme/no-raw-sql`。

## 某个工具跳过的文件

`[linter]`、`[analyzer]` 或 `[guard]` 下的 `excludes` 只把文件从该工具中移除。`[source].excludes` 把文件从所有工具中移除。

```toml
[guard]
excludes = ["src/ThirdParty/**"]
```

## 已有问题

[基线](/fundamentals/baseline/)记录项目当前已有的问题,这样在修复旧问题期间,每次运行只报告新问题。
