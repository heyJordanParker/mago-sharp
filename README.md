<p align="center">
  <img src="docs/static/img/banner.svg" alt="Mago Banner" width="600" />
</p>

<div align="center">

**An extremely fast PHP linter, formatter, and static analyzer, written in Rust.**

</div>

<div align="center">

[![CI Status](https://github.com/heyJordanParker/mago-sharp/actions/workflows/ci.yml/badge.svg)](https://github.com/heyJordanParker/mago-sharp/actions/workflows/ci.yml)
[![CD Status](https://github.com/heyJordanParker/mago-sharp/actions/workflows/cd.yml/badge.svg)](https://github.com/heyJordanParker/mago-sharp/actions/workflows/cd.yml)
[![Latest Stable Version for PHP](https://poser.pugx.org/heyjordanparker/mago-sharp/v)](https://packagist.org/packages/heyjordanparker/mago-sharp)
[![Total Composer Downloads](http://poser.pugx.org/heyjordanparker/mago-sharp/downloads)](https://packagist.org/packages/heyjordanparker/mago-sharp)
[![License](https://img.shields.io/github/license/heyJordanParker/mago-sharp)](https://github.com/heyJordanParker/mago-sharp/blob/master/LICENSE-MIT)

</div>

**Mago** is a comprehensive toolchain for PHP that helps developers write better code. Inspired by the Rust ecosystem, Mago brings speed, reliability, and an exceptional developer experience to PHP projects of all sizes.

## Table of Contents

- [Installation](#installation)
- [Getting Started](#getting-started)
- [Features](#features)
- [Contributing](#contributing)
- [Inspiration & Acknowledgements](#inspiration--acknowledgements)
- [License](#license)

## Installation

The most common way to install Mago on macOS and Linux is by using our shell script:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/mago-sharp/master/scripts/install.sh | bash
```

To install a specific version:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/mago-sharp/master/scripts/install.sh | bash -s -- --version=0.3.1
```

For all other installation methods, including Composer, please refer to the **[Installation Guide](docs/content/en/guide/installation.md)**.

## Getting Started

To get started with Mago and learn how to configure your project, please visit the **[Getting Started Guide](docs/content/en/guide/getting-started.md)**.

## Features

- ⚡️ Extremely Fast: Built in Rust for maximum performance.
- 🔍 Lint: Identify issues in your codebase with customizable rules.
- 🔬 Static Analysis: Perform deep analysis of your codebase to catch potential type errors and bugs.
- 🛠️ Automated Fixes: Apply fixes for many lint issues automatically.
- 📜 Formatting: Automatically format your code to adhere to best practices and style guides.
- 🧠 Semantic Checks: Ensure code correctness with robust semantic analysis.
- 🌳 CST Visualization: Explore your code’s structure with Concrete Syntax Tree (CST) parsing.

## Contributing

Contributions are welcome.

- See our [Contributing Guide](./CONTRIBUTING.md) to get started.

## Inspiration & Acknowledgements

Mago stands on the shoulders of giants. Our design and functionality are heavily inspired by pioneering tools in both the Rust and PHP ecosystems.

### Inspirations:

- [Clippy](https://github.com/rust-lang/rust-clippy): For its comprehensive linting approach.
- [OXC](https://github.com/oxc-project/oxc/): A major inspiration for building a high-performance toolchain in Rust.
- [Hakana](https://github.com/slackhq/hakana/): For its deep static analysis capabilities.

### Acknowledgements:

We deeply respect the foundational work of tools like [PHP-CS-Fixer](https://github.com/PHP-CS-Fixer/PHP-CS-Fixer), [Psalm](https://github.com/vimeo/psalm), [PHPStan](https://github.com/phpstan/phpstan), and [PHP_CodeSniffer](https://github.com/PHPCSStandards/PHP_CodeSniffer). While Mago aims to offer a unified and faster alternative, these tools paved the way for modern PHP development.

## License

Mago is dual-licensed under your choice of the following:

- MIT License ([LICENSE-MIT](./LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
