<?php

declare(strict_types=1);

/**
 * Prints the class-likes a `.sharp` file names with no import, read from php-sharp's standard library,
 * `sharp/composer/library`, and the prelude's `crates/prelude/assets/extensions/sharp.php`:
 *
 * - the class-likes declared directly in `Sharp`, spec section 23: each public one of `Sharp/*.sharp`, and each one of
 *   the prelude;
 * - the standard root classes, spec section 23: each class `Sharp/Internal/Builtins.sharp` declares with no import,
 *   and each class of `sharp/bin/builtins`'s `STANDARD_NAMESPACED_CLASSES`, which it imports;
 * - the standard effects, spec section 29: each `effect` class of the standard library, in any of its namespaces;
 * - the class marked `EffectOfEmit`, the effect of `emit`, and the interface marked `PropertyBehavior`, the one a
 *   property behavior implements.
 *
 * `just regen-sharp-classes <path to sharp/composer/library>` writes it to `crates/names/src/sharp_classes.rs`.
 */

$library = $argv[1] ?? throw new InvalidArgumentException('Pass the path to php-sharp\'s `sharp/composer/library`.');
if (count($argv) > 2) {
    throw new InvalidArgumentException('Pass only the path to php-sharp\'s `sharp/composer/library`.');
}
$library = rtrim($library, '/');

function read(string $file): string
{
    $source = file_get_contents($file);
    if ($source === false) {
        throw new RuntimeException("Unable to read {$file}.");
    }

    return $source;
}

function namespace_of(string $file, string $source): string
{
    $match = [];
    if (preg_match('/^namespace (Sharp(?:\.\w+)*);$/m', $source, $match) !== 1) {
        throw new RuntimeException("{$file} is in no namespace of the standard library.");
    }

    return str_replace('.', '\\', $match[1] ?? '');
}

/**
 * @param list<string> $classes
 */
function rust(string $visibility, string $name, string $documentation, array $classes): string
{
    $classes = array_unique($classes);
    sort($classes, SORT_STRING);
    $count = count($classes);
    $entries = implode('', array_map(
        static fn(string $class): string => '    "' . str_replace('\\', '\\\\', $class) . "\",\n",
        $classes,
    ));

    return "{$documentation}\n{$visibility} const {$name}: [&str; {$count}] = [\n{$entries}];\n";
}

$files = glob("{$library}/Sharp/*.sharp");
if ($files === false || $files === []) {
    throw new RuntimeException("Unable to find the standard library's files in {$library}/Sharp.");
}

$classes = [];
foreach ($files as $file) {
    $source = read($file);
    if (namespace_of($file, $source) !== 'Sharp') {
        throw new RuntimeException("{$file} is not in the namespace `Sharp`.");
    }

    $declarations = [];
    preg_match_all('/^public\s+(?:\w+\s+)*?(?:class|interface|enum|struct)\s+(\w+)/m', $source, $declarations);
    array_push($classes, ...array_map(static fn(string $class): string => "Sharp\\{$class}", $declarations[1]));
}

$prelude = dirname(__DIR__) . '/crates/prelude/assets/extensions/sharp.php';
$namespace = [];
preg_match('/^namespace Sharp;$(.*?)(?=^namespace |\z)/ms', read($prelude), $namespace);
$body = $namespace[1] ?? throw new RuntimeException("Unable to locate `namespace Sharp;` in {$prelude}.");
$declarations = [];
preg_match_all('/^(?:\w+\s+)*?(?:class|interface|enum|trait)\s+(\w+)/m', $body, $declarations);
array_push($classes, ...array_map(static fn(string $class): string => "Sharp\\{$class}", $declarations[1]));

$builtins = read("{$library}/Sharp/Internal/Builtins.sharp");
$imports = [];
preg_match_all('/^import ([\w.]+)(?: as (\w+))?;$/m', $builtins, $imports, PREG_SET_ORDER);
// Each imported class by the name `Builtins.sharp` gives it, its alias or its short name, to its full name.
$imported = [];
foreach ($imports as $import) {
    $full = $import[1] ?? '';
    $imported[$import[2] ?? basename(str_replace('.', '/', $full))] = str_replace('.', '\\', $full);
}
$externs = [];
preg_match_all('/^extern (\w+) uses [\w, ]+;$/m', $builtins, $externs);
// Each root class `Builtins.sharp` declares with no import, by its name, and each class of a namespace that
// `sharp/bin/builtins` lists in `STANDARD_NAMESPACED_CLASSES`, which it imports and a `.sharp` file names through its
// import, by its full name. A class of a namespace it imports only to declare its effects, as
// `Random\RandomException`, stays out, and so does a root class it imports, as `DateTimeImmutable`.
$generator = read(dirname($library, 2) . '/bin/builtins');
$listed = [];
preg_match('/^const STANDARD_NAMESPACED_CLASSES = \[(.*?)\];$/ms', $generator, $listed);
$list = $listed[1] ?? throw new RuntimeException('sharp/bin/builtins declares no STANDARD_NAMESPACED_CLASSES.');
$namespaced = array_map(
    static fn(PhpToken $token): string => stripslashes(substr($token->text, 1, -1)),
    array_values(array_filter(PhpToken::tokenize("<?php {$list}"), static fn(PhpToken $token): bool => $token->is(
        T_CONSTANT_ENCAPSED_STRING,
    ))),
);
foreach ($namespaced as $class) {
    if (!in_array($class, $imported, true)) {
        throw new RuntimeException("Builtins.sharp does not import {$class}, which STANDARD_NAMESPACED_CLASSES lists.");
    }
}
$roots = [...array_diff($externs[1], array_keys($imported)), ...$namespaced];

$effects = [];
$marked = ['EffectOfEmit' => [], 'PropertyBehavior' => []];
$sources = new RecursiveIteratorIterator(
    new RecursiveDirectoryIterator("{$library}/Sharp", FilesystemIterator::SKIP_DOTS),
);
foreach ($sources as $entry) {
    if (!$entry instanceof SplFileInfo || $entry->getExtension() !== 'sharp') {
        continue;
    }
    $path = $entry->getPathname();
    $source = read($path);
    $declarations = [];
    preg_match_all('/^public effect class (\w+)/m', $source, $declarations);
    foreach ($declarations[1] as $class) {
        $effects[] = namespace_of($path, $source) . "\\{$class}";
    }
    $marks = [];
    preg_match_all(
        '/^\[(EffectOfEmit|PropertyBehavior)\]\n(?:\[.*\]\n)*public\s+(?:\w+\s+)*?(?:class|interface)\s+(\w+)/m',
        $source,
        $marks,
        PREG_SET_ORDER,
    );
    foreach ($marks as $mark) {
        $marked[$mark[1] ?? ''][] = namespace_of($path, $source) . '\\' . ($mark[2] ?? '');
    }
}
foreach ($marked as $mark => $holders) {
    if (count($holders) !== 1) {
        throw new RuntimeException("The standard library marks `{$mark}` on " . count($holders) . ' classes, not one.');
    }
}

echo "//! Generated by `just regen-sharp-classes` from php-sharp's standard library and the prelude's\n";
echo "//! `assets/extensions/sharp.php`.\n";
echo "//! Do not edit.\n\n";
echo
    rust(
        'pub(crate)',
        'SHARP_CLASSES',
        "/// The class-likes declared directly in `Sharp`: each public one of the standard library, and each one of the\n/// prelude. Sorted.",
        $classes,
    );
echo "\n";
echo
    rust(
        'pub',
        'STANDARD_ROOT_CLASSES',
        "/// The PHP built-in classes a `.sharp` file names, spec section 23: each root one by a bare name by default, and each\n/// of a namespace, as `Dom\\HTMLDocument`, through its import.\n///\n/// `Builtins.sharp` declares each: the exception classes, `Throwable`, `Attribute`, the interfaces a PHP# class\n/// implements for a plain PHP library, the classes a plain PHP library hands PHP# code, and the DOM that reads and\n/// writes markup. Sorted.",
        $roots,
    );
echo "\n";
echo
    rust(
        'pub(crate)',
        'STANDARD_EFFECTS',
        "/// The standard effects, spec section 29: each `effect` class of the standard library, which a bare name in `uses`\n/// names by default, as a bare name of `Sharp` names its class. Sorted.",
        $effects,
    );
$emit = str_replace('\\', '\\\\', $marked['EffectOfEmit'][0]);
$behavior = str_replace('\\', '\\\\', $marked['PropertyBehavior'][0]);
echo "\n";
echo "/// The class the standard library marks `Sharp\\Internal\\EffectOfEmit`: the effect of `emit`, spec section\n";
echo "/// 29.\n";
echo "pub const EMIT_EFFECT: &str = \"{$emit}\";\n";
echo "\n";
echo "/// The interface the standard library marks `Sharp\\Internal\\PropertyBehavior`: the one a property behavior\n";
echo "/// implements, spec section 6.4.\n";
echo "pub const PROPERTY_BEHAVIOR: &str = \"{$behavior}\";\n";
