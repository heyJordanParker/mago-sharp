#!/bin/sh
# Counts the instructions the front end spends on a generated class of 10,000 and of 20,000 methods, as `.sharp`
# through the checker, the analysis and `lower` and as `.php` through the checker, and the instructions PHP's own
# compile spends on the `.php` class through `php -l`. Each count is the "instructions retired" of `/usr/bin/time -l`
# on macOS, three runs each. The first lines count each process on an empty file, the startup every later count
# includes, the prelude among it. The `.sharp` class is also counted stopped after parsing, after binding names and
# after the semantic checks, so each difference is one pass, and `lower .sharp` adds the analysis and the lowering. A
# third class, whose methods each read a constant, counts the lookup of a bare name among the class's members.
#
# A folder argument, such as Laravel's `src`, counts the checker and `php -l` on every `.php` file in it, each in one
# process. Each command first runs once on its own, so a failing file stops the bench before any count.
#
# Usage: crates/sharp-bridge/bench.sh [methods or folder...]

set -eu

cd "$(dirname "$0")/../.."
rustup run 1.97.0 cargo build --quiet --profile memory-debug -p mago-sharp-bridge --example front_end

front_end=target/memory-debug/examples/front_end
classes=$(mktemp -d)
trap 'rm -rf "$classes"' EXIT

# Prints `label` and three instruction counts of the command after it.
count() {
    label=$1
    shift
    "$@" > /dev/null
    printf '  %-20s' "$label"
    for run in 1 2 3; do
        /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ { printf " %.3fe9", $1 / 1e9 }'
    done
    echo
}

: > "$classes/Empty.sharp"
echo "<?php" > "$classes/Empty.php"
echo "empty file, the startup of each process:"
count "lower .sharp:" "$front_end" "$classes/Empty.sharp"
count "front end .php:" "$front_end" "$classes/Empty.php"
count "php -l .php:" php -l "$classes/Empty.php"

[ $# -gt 0 ] || set -- 10000 20000
for argument in "$@"; do
    if [ -d "$argument" ]; then
        find "$argument" -name '*.php' | sort > "$classes/files"
        echo "$(wc -l < "$classes/files" | tr -d ' ') .php files in $argument:"
        # shellcheck disable=SC2046 # one argument per path, and no path holds a space
        count "front end .php:" "$front_end" $(cat "$classes/files")
        # shellcheck disable=SC2046
        count "php -l .php:" php -l $(cat "$classes/files")
        continue
    fi

    methods=$argument
    awk -v methods="$methods" 'BEGIN {
        print "namespace App;\n\nclass Big\n{"
        for (i = 0; i < methods; i++) printf "    public int m%d(int value)\n    {\n        return value + %d;\n    }\n\n", i, i
        print "}"
    }' > "$classes/Big$methods.sharp"
    awk -v methods="$methods" 'BEGIN {
        print "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nclass Big\n{"
        for (i = 0; i < methods; i++) printf "    public function m%d(int $value): int\n    {\n        return $value + %d;\n    }\n\n", i, i
        print "}"
    }' > "$classes/Big$methods.php"

    echo "$methods methods, $(wc -c < "$classes/Big$methods.sharp" | tr -d ' ') bytes of PHP#:"
    count "parse .sharp:" "$front_end" --until parse "$classes/Big$methods.sharp"
    count "+ names .sharp:" "$front_end" --until names "$classes/Big$methods.sharp"
    count "+ checks .sharp:" "$front_end" --until checks "$classes/Big$methods.sharp"
    count "lower .sharp:" "$front_end" "$classes/Big$methods.sharp"
    count "front end .php:" "$front_end" "$classes/Big$methods.php"
    count "php -l .php:" php -l "$classes/Big$methods.php"

    # A bare name that is not a local is looked up among the class's members, once per read.
    awk -v methods="$methods" 'BEGIN {
        print "namespace App;\n\nclass Big\n{"
        for (i = 0; i < methods; i++) printf "    public int m%d(int value)\n    {\n        return value + PHP_INT_SIZE;\n    }\n\n", i
        print "}"
    }' > "$classes/Constants$methods.sharp"

    echo "$methods methods that each read a constant:"
    count "lower .sharp:" "$front_end" "$classes/Constants$methods.sharp"
done
