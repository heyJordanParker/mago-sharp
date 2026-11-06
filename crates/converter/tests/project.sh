#!/usr/bin/env bash
set -uo pipefail

# The whole-project proof of mago convert, which sharp/bin/test runs after the PHP# suite. Copies the fixture project
# into the empty folder $1, installs it, commits it to a fresh git repository and converts the files below, then checks
# that:
# - `--dry-run` changes no file
# - each file mago convert refuses stays plain PHP, and every other file converts
# - the index holds each conversion as a rename alone, `R100`, and `git log --follow` reaches the plain PHP file
# - `mago compile` accepts every file, the plain PHP subclasses of converted classes included
# - `mago analyze` has no new error, and only the warnings decision 9 expects
# - run.php prints the same output on the engine before and after
# - a class hierarchy converts in either order, and compiles and runs the same after each file
# Needs the engine and mago built by sharp/bin/build and sharp/bin/build-mago, and Composer and a php on PATH for the
# refinement worker, whose SDK packages it installs. Writes each step's output beside $1. Exits 1 at the first check that fails.
root="$(cd "$(dirname "$0")/../../../../.." && pwd)"
build="$root/sharp/build/$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
engine="$build/sapi/cli/php"
mago=("$build/mago/test-run/mago" --no-version-check --colors never)
project="$1"
converted=(src/Box.php src/Spec.php src/Shop/Crate.php src/Shop/Label.php src/Shop/Note.php src/Shop/Order.php
    src/Shop/Shelf.php src/Shop/Stock.php src/Shop/Tagged.php src/Shop/Tally.php)
plain=(src/Factory.php src/Shop/Counter.php src/Shop/Feed.php src/Shop/Ledger.php src/Shop/Pair.php)
expected_warnings='too-wide-member|redundant-docblock-type'

# Prints the command on stderr, so a file its output is saved in holds the output alone.
step() { printf '\n$ %s\n' "${*/#$build\//}" >&2; "$@"; }
fail() { printf '\nFAILED: %s\n' "$*"; exit 1; }
commit() { git -c user.name=proof -c user.email=proof@localhost commit --quiet "$@"; }

# The refinement worker loads the SDK through sharp/mago's own Composer packages, as sharp/bin/coverage installs them.
composer install --no-interaction --no-progress --quiet --working-dir="$root/sharp/mago" || fail "composer install of the SDK"
cp -R "$(dirname "$0")/project/." "$project"
cd "$project" || exit 1
# BSD and GNU sed both take -i with a suffix joined to it.
sed -i.plain "s|\"../../../../../composer\"|\"$root/sharp/composer\"|" composer.json
sed -i.plain "s|\"../../../../composer/|\"$root/sharp/mago/composer/|" mago.toml
rm composer.json.plain mago.toml.plain
composer install --quiet --no-interaction || fail "composer install"
git init --quiet
git add --all
commit -m "plain PHP"

step "$engine" -n run.php | tee ../run.before || fail "run.php before"
step "${mago[@]}" analyze --reporting-format emacs | tee ../analyze.before
step "${mago[@]}" lint --reporting-format emacs "${converted[@]}"
step "${mago[@]}" guard --reporting-format emacs "${converted[@]}"

step git status --short --untracked-files=all | tee ../status.before
step "${mago[@]}" convert --dry-run src > ../dry-run 2>&1
cat ../dry-run
step git status --short --untracked-files=all | tee ../status.after
diff ../status.before ../status.after || fail "--dry-run changed a file"
for file in "${converted[@]}"; do
    grep -q "^$file -> ${file%.php}.sharp\$" ../dry-run || fail "--dry-run shows no diff for $file"
done

step "${mago[@]}" convert src/Spec.php src/Box.php src/Factory.php src/Shop 2>&1 | tee ../convert
[ "${PIPESTATUS[0]}" = 1 ] || fail "mago convert exits 0 although files stay plain PHP"
for file in "${plain[@]}"; do
    grep -q "$file stays plain PHP:" ../convert || fail "$file converted"
    [ -f "$file" ] || fail "$file is gone"
done
for file in "${converted[@]}"; do
    [ -f "${file%.php}.sharp" ] || fail "$file did not convert"
    [ ! -f "$file" ] || fail "$file is still there"
done
grep -q "Converted ${#converted[@]} files to PHP#, and ${#plain[@]} files stay plain PHP." ../convert || fail "the summary"

step git diff --cached -M --stat
step git diff --cached -M --name-status | tee ../renames
[ "$(grep -c '^R100' ../renames)" = "${#converted[@]}" ] || fail "the index holds more than one rename per file"
[ "$(wc -l < ../renames | tr -d ' ')" = "${#converted[@]}" ] || fail "the index holds more than the renames"

step "${mago[@]}" compile || fail "mago compile refuses a file"
step git status --short --ignored src
commit -m "rename to PHP#"
commit --all -m "convert to PHP#"
step git log --follow --name-status --format=%s -- src/Shop/Shelf.sharp | tee ../history
grep -q "^R100	src/Shop/Shelf.php	src/Shop/Shelf.sharp\$" ../history || fail "git log --follow loses the plain PHP file"

step "${mago[@]}" analyze --reporting-format emacs | tee ../analyze.after
grep ':error - ' ../analyze.after | while read -r error; do
    grep -qxF "$error" ../analyze.before || fail "a new error: $error"
done || exit 1
grep ':warning - ' ../analyze.after | while read -r warning; do
    grep -qxF "$warning" ../analyze.before || grep -qE ":warning - ($expected_warnings):" <<< "$warning" ||
        fail "a warning decision 9 doesn't expect: $warning"
done || exit 1

step "$engine" -n run.php | tee ../run.after || fail "run.php after"
step diff ../run.before ../run.after || fail "run.php prints another output"

# A class hierarchy converts in either order: a subclass before its parent, then the parent under a converted and a
# plain subclass, then the plain subclass under its converted parent. Each step compiles and runs as before.
for file in src/Zoo/Cat.php src/Zoo/Animal.php src/Zoo/Bird.php; do
    step "${mago[@]}" convert "$file" || fail "mago convert $file"
    [ -f "${file%.php}.sharp" ] || fail "$file did not convert"
    step "${mago[@]}" compile || fail "mago compile refuses a file after $file converts"
    step "$engine" -n run.php > ../run.hierarchy || fail "run.php after $file converts"
    step diff ../run.before ../run.hierarchy || fail "run.php prints another output after $file converts"
done

printf '\nThe whole-project proof passes.\n'
