+++
title = "GitHub Actions 实用方案"
description = "在每次 push 和 pull request 上运行格式化、lint 和静态分析。"
nav_order = 50
nav_section = "实用方案"
+++
# GitHub Actions 实用方案

一个简单的工作流,在每次 push 和 pull request 上运行格式化器、linter 和分析器,并附带原生 PR 注解。

## 快速配置

该工作流通过 Composer 安装 Mago,所以先把它加入你的项目:

```sh
composer require --dev "heyjordanparker/mago-sharp:^0.3.1"
```

然后创建 `.github/workflows/mago.yml`:

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

关于结构的几点说明:

- 把 `php-version` 设为你的项目所用的 PHP 版本。`composer install` 同时为分析器提供依赖,分析器需要它们来解析符号。
- 第一次调用 `vendor/bin/mago` 会从 GitHub 发布下载对应的二进制。`GITHUB_TOKEN` 不会自动导出给各个步骤,所以该任务显式传入它,以避开共享 runner 上 GitHub 的匿名速率限制。
- 把 `format`、`lint` 和 `analyze` 拆成独立步骤,可以在某个步骤失败时仍呈现其余两步的结果。把它们合并到单个 `run:` 中会在第一个失败处短路,导致后面的输出被隐藏。
- `if: success() || failure()` 会在任务未被取消时运行该步骤,这正是我们想要的。`always()` 在配置阶段失败后也会运行该步骤。
- 使用 `mago format --check`,而不是 `--dry-run`。`--check` 在有文件需要格式化时以非零状态退出;`--dry-run` 仅打印 diff,始终以零状态退出。
- Mago 会通过 `GITHUB_ACTIONS` 环境变量识别 GitHub Actions,自动切换到 `--reporting-format=github`,产出原生 PR 注解。无需额外配置。
