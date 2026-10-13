+++
title = "常见问题"
description = "关于 Mago 项目以及哪些内容属于、哪些不属于本项目的常见问题。"
nav_order = 10
nav_section = "参考"
+++
# 常见问题

## 为什么叫 "Mago"?

这个名字来自上游项目 Mago。上游最初叫 "fennec",取自北非的耳廓狐。由于与另一款工具发生命名冲突,被迫更名。

上游的维护者 Carthage Software 以 Mago of Carthage 为它命名。Mago of Carthage 是古迦太基的一位作家,被誉为"农业之父"。正如他耕耘土地,这款工具也旨在帮助开发者耕耘自己的代码库。

这个名字还有一层有用的双关含义。在西班牙语和意大利语中,"mago" 意为"魔术师"或"巫师"。为上游 Mago 设计的 logo 同时呈现这两层含义:一只穿着巫师帽和长袍的耳廓狐,衣服上绣着古迦太基的塔尼特(Tanit)符号。

## Mago 怎么读?

`/ˈmɑːɡoʊ/`,"mah-go"。两个音节:"ma" 像 "mama" 中的发音,"go" 像 "go" 的发音。

## mago-sharp 提供语言服务器吗?

不提供。mago-sharp 没有实现 Language Server Protocol。请在命令行或 CI 中使用它。[配置](/guide/configuration/)页面介绍了编辑器用来校验 `mago.toml` 的 JSON Schema,以及在编辑器中打开报告文件的终端链接。

## mago-sharp 提供编辑器扩展(VS Code 等)吗?

不提供。mago-sharp 不提供任何编辑器专用扩展。

## mago-sharp 支持分析器插件吗?

支持,通过扩展实现。扩展是一个外部程序,mago-sharp 根据 `mago.toml` 中的 `[extension-hosts]` 启动它,并通过二进制 worker 协议与之通信。扩展可以添加 linter 规则和分析器插件。格式化器和 guard 没有扩展 API。mago-sharp 提供用于编写扩展的 PHP SDK,即 Composer 包 `heyjordanparker/mago-sharp` 中的 `Mago\Sdk` 命名空间。[扩展](/extensions/overview/)页面介绍了该协议、SDK 和一个完整示例。

## mago-sharp 包含哪些工具?

一个二进制文件提供:

- `mago lint`:linter。
- `mago analyze`:静态分析器,支持 PHP 和 PHP#。
- `mago format`:格式化器。
- `mago guard`:强制执行架构分层规则。
- `mago compile`:为 PHP# 引擎编译 PHP# 文件。

`mago fix` 会反复应用 guard、分析器、linter 和格式化器的修复,直到它们都不再产生变化。

## Mago 会实现一个 Composer 替代品吗?

不会。Composer 是一款出色的工具,而它的大部分工作都是 I/O 密集型的。Rust 重写不会带来多少速度提升,反而会割裂生态,而且很难支持 Composer 基于 PHP 的插件架构。

## Mago 会实现一个 PHP 运行时吗?

不会。PHP 运行时极为庞大。即便是非常大规模的尝试(Facebook 的 HHVM、VK 的 KPHP)也难以与 Zend 引擎完全对齐。一个更小的项目无法做得更好,而结果只会让社区进一步割裂。Mago 专注于工具链,不涉足运行时。
