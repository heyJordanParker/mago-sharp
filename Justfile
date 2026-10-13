template_dir := `mktemp -d`

# Lists all available commands.
list:
    @just --list

# Regenerate the analyzer issue codes.
regen-analyzer-issue-codes:
    rm -f crates/analyzer/src/code.rs
    php scripts/regen-analyzer-issue-codes.php >> crates/analyzer/src/code.rs
    rustfmt crates/analyzer/src/code.rs

# Regenerate the PHP SDK's stable syntax node names.
regen-sdk-node-kinds:
    php scripts/regen-sdk-node-kinds.php

# Regenerate the PHP# bridge's `sharp_kind`, `SHARP_T_*` tokens and `SHARP_UNIT_ABI` from php-sharp's `Zend/zend_ast.h` and `Zend/zend_language_parser.h`.
regen-sharp-kinds zend_ast_h zend_language_parser_h:
    php scripts/regen-sharp-kinds.php {{zend_ast_h}} {{zend_language_parser_h}} > crates/sharp-bridge/src/kind.rs

# Copy the PHP# bridge's C header, `sharp_unit.h`, which php-sharp commits as `ext/sharp/sharp_unit.h`.
sharp-header destination:
    #!/usr/bin/env php
    <?php
    $messages = shell_exec('cargo build --locked -p mago-sharp-bridge --message-format=json') ?: '';
    foreach (explode("\n", $messages) as $line) {
        $message = json_decode($line, true);
        if (($message['reason'] ?? '') === 'build-script-executed' && str_contains($message['package_id'], 'mago-sharp-bridge')) {
            exit(copy($message['out_dir'] . '/sharp_unit.h', '{{destination}}') ? 0 : 1);
        }
    }
    fwrite(STDERR, "cargo reported no out_dir for mago-sharp-bridge\n");
    exit(1);

# Builds the library in release mode.
build:
    cargo build --release

# Builds the webassembly module.
build-wasm:
    cd crates/wasm && wasm-pack build --release --out-dir pkg

# Detects problems using rustfmt, clippy, and cargo check, and runs the linter and analyzer.
check:
    cargo run -- fmt --check
    cargo run -- lint
    cargo run -- analyze
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo check --workspace --locked

# Fixes linting problems automatically using clippy, cargo fix, and rustfmt.
fix:
    cargo run -- lint --fix
    cargo run -- analyze --fix
    cargo run -- fmt
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged
    cargo fix --allow-dirty --allow-staged
    cargo fmt --all

# Runs all tests in the workspace.
test:
    cargo test --workspace --locked --all-targets

# Runs the PHP SDK test suite.
test-sdk:
    vendor/bin/phpunit --configuration composer/phpunit.xml

# Fuzz the PHP lexer. Seeds are drawn from the syntax/analyzer/formatter PHP fixtures.
fuzz-php-lexer:
    cd crates/syntax/fuzz && cargo +nightly fuzz run lexer \
        corpus/lexer \
        ../tests/fixtures \
        ../../analyzer/tests/cases \
        ../../formatter/tests/cases

# Fuzz the PHP parser. Seeds are drawn from the syntax/analyzer/formatter PHP fixtures.
fuzz-php-parser:
    cd crates/syntax/fuzz && cargo +nightly fuzz run parser \
        corpus/parser \
        ../tests/fixtures \
        ../../analyzer/tests/cases \
        ../../formatter/tests/cases

# Fuzz the Twig lexer.
fuzz-twig-lexer:
    cd crates/twig-syntax/fuzz && cargo +nightly fuzz run lexer corpus/lexer seeds/lexer

# Fuzz the Twig parser.
fuzz-twig-parser:
    cd crates/twig-syntax/fuzz && cargo +nightly fuzz run parser corpus/parser seeds/parser

# Publishes all crates to crates.io in the correct order.
publish:
    # Note: the order of publishing is important, as some crates depend on others.
    cargo publish -p mago-bytes
    cargo publish -p mago-word
    cargo publish -p mago-allocator
    cargo publish -p mago-casing
    cargo publish -p mago-php-version
    cargo publish -p mago-text-edit
    cargo publish -p mago-database
    cargo publish -p mago-span
    cargo publish -p mago-reporting
    cargo publish -p mago-syntax-core
    cargo publish -p mago-syntax
    cargo publish -p mago-phpdoc-syntax
    cargo publish -p mago-twig-syntax
    cargo publish -p mago-flags
    cargo publish -p mago-hir
    cargo publish -p mago-collector
    cargo publish -p mago-composer
    cargo publish -p mago-formatter
    cargo publish -p mago-names
    cargo publish -p mago-semantics
    cargo publish -p mago-fingerprint
    cargo publish -p mago-codex
    cargo publish -p mago-prelude
    cargo publish -p mago-algebra
    cargo publish -p mago-guard
    cargo publish -p mago-extension
    cargo publish -p mago-analyzer
    cargo publish -p mago-linter
    cargo publish -p mago-orchestrator
    cargo publish -p mago-wasm
    cargo publish

# Cleans all build artifacts from the workspace.
clean:
    cargo clean --workspace
