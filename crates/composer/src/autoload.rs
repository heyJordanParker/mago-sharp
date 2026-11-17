//! Composer's autoload maps: where each class of an installed package lives, as `vendor/composer/autoload_*.php`
//! writes it, and the files Composer loads on every request.

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;

use foldhash::HashMap;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::MagicConstant;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Variable;
use mago_syntax::parser::parse_file;

/// The maps Composer writes in its folder of the vendor folder, `vendor/composer`, which [`Autoload::read`] reads: the
/// class map, the PSR-4 prefixes, the PSR-0 prefixes and the files loaded up front.
pub const MAPS: [&str; 4] =
    ["autoload_classmap.php", "autoload_psr4.php", "autoload_namespaces.php", "autoload_files.php"];

/// The extensions a class file may carry, in the order the autoloader tries them: PHP's, then PHP#'s, which the PHP#
/// Composer plugin tries after Composer's own.
const EXTENSIONS: [&str; 2] = ["php", "sharp"];

/// Composer's autoload maps of one vendor folder: its class map, its PSR-4 and PSR-0 prefixes, and the files it loads
/// up front.
#[derive(Debug, Default)]
pub struct Autoload {
    class_map: HashMap<Vec<u8>, PathBuf>,
    /// Each PSR-4 prefix, ending in `\`, with its folders, and the folders of the empty prefix.
    psr4: HashMap<Vec<u8>, Vec<PathBuf>>,
    psr4_fallback: Vec<PathBuf>,
    /// Each PSR-0 prefix with its folders, in the order the map writes them, and the folders of the empty prefix.
    psr0: Vec<(Vec<u8>, Vec<PathBuf>)>,
    psr0_fallback: Vec<PathBuf>,
    files: Vec<PathBuf>,
}

/// Why the autoload maps of a vendor folder could not be read.
#[derive(Debug)]
pub enum AutoloadError {
    /// The file system failed to read the map or the `composer.json` at `path`.
    Read { path: PathBuf, error: std::io::Error },
    /// The map at `path` holds code other than what Composer writes, at `line`.
    Unsupported { path: PathBuf, line: u32 },
    /// The `composer.json` at `path` holds no JSON.
    Manifest { path: PathBuf, error: serde_json::Error },
}

impl std::fmt::Display for AutoloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "Failed to read `{}`: {error}", path.display()),
            Self::Unsupported { path, line } => write!(
                f,
                "Composer's autoload map `{}` holds code Composer doesn't write, on line {line}. Run `composer dump-autoload` to write it again.",
                path.display()
            ),
            Self::Manifest { path, error } => write!(f, "Failed to parse `{}`: {error}", path.display()),
        }
    }
}

impl std::error::Error for AutoloadError {}

/// Composer's vendor folder of the project in `workspace`: the folder `composer.json`'s `config.vendor-dir` names, else
/// `vendor`.
///
/// # Errors
///
/// Returns an [`AutoloadError`] when `composer.json` can't be read or holds no JSON.
pub fn vendor_folder(workspace: &Path) -> Result<PathBuf, AutoloadError> {
    let manifest = workspace.join("composer.json");
    let contents = match std::fs::read(&manifest) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(workspace.join("vendor")),
        Err(error) => return Err(AutoloadError::Read { path: manifest, error }),
    };
    let package: serde_json::Value =
        serde_json::from_slice(&contents).map_err(|error| AutoloadError::Manifest { path: manifest, error })?;
    let folder = package.pointer("/config/vendor-dir").and_then(serde_json::Value::as_str).unwrap_or("vendor");

    Ok(workspace.join(folder))
}

impl Autoload {
    /// The maps of `vendor`, Composer's vendor folder, or `None` when Composer has written none there: when
    /// `vendor/composer/autoload_psr4.php` is missing.
    ///
    /// # Errors
    ///
    /// Returns an [`AutoloadError`] when a map can't be read or holds code Composer doesn't write.
    pub fn read(vendor: &Path) -> Result<Option<Self>, AutoloadError> {
        let folder = vendor.join("composer");
        if !folder.join(MAPS[1]).is_file() {
            return Ok(None);
        }

        let [class_map, psr4, psr0, files] = MAPS;
        let mut autoload = Self::default();
        for (class, file) in read_map(&folder, class_map)? {
            autoload.class_map.insert(class, path(file));
        }
        for (prefix, folders) in read_map(&folder, psr4)? {
            let folders = paths(folders);
            if prefix.is_empty() {
                autoload.psr4_fallback = folders;
            } else {
                autoload.psr4.insert(prefix, folders);
            }
        }
        for (prefix, folders) in read_map(&folder, psr0)? {
            let folders = paths(folders);
            if prefix.is_empty() {
                autoload.psr0_fallback = folders;
            } else {
                autoload.psr0.push((prefix, folders));
            }
        }
        autoload.files = read_map(&folder, files)?.into_iter().map(|(_, file)| path(file)).collect();

        Ok(Some(autoload))
    }

    /// The file Composer's autoloader loads for `class`, a fully qualified name with or without its leading `\`, or
    /// `None` when no map places it on disk. It looks as Composer's `ClassLoader` does: the class map, then the PSR-4
    /// prefixes from the longest, then the PSR-0 ones, each with `.php` and then `.sharp`.
    #[must_use]
    pub fn locate(&self, class: &[u8]) -> Option<PathBuf> {
        let class = class.strip_prefix(b"\\").unwrap_or(class);
        if let Some(file) = self.class_map.get(class) {
            return Some(file.clone());
        }

        EXTENSIONS.iter().find_map(|extension| self.locate_with(class, extension))
    }

    /// The files Composer loads on every request, `autoload_files.php`'s, in its order.
    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    fn locate_with(&self, class: &[u8], extension: &str) -> Option<PathBuf> {
        let class = String::from_utf8_lossy(class);
        let psr4_path = format!("{}.{extension}", class.replace('\\', "/"));
        let existing = |folder: &PathBuf, relative: &str| Some(folder.join(relative)).filter(|file| file.is_file());

        let mut prefix_end = class.len();
        while let Some(separator) = class[..prefix_end].rfind('\\') {
            prefix_end = separator;
            if let Some(folders) = self.psr4.get(class[..=separator].as_bytes())
                && let Some(file) = folders.iter().find_map(|folder| existing(folder, &psr4_path[separator + 1..]))
            {
                return Some(file);
            }
        }

        if let Some(file) = self.psr4_fallback.iter().find_map(|folder| existing(folder, &psr4_path)) {
            return Some(file);
        }

        let short = class.rfind('\\').map_or(0, |separator| separator + 1);
        let psr0_path = format!("{}{}.{extension}", &psr4_path[..short], class[short..].replace('_', "/"));
        self.psr0
            .iter()
            .filter(|(prefix, _)| class.as_bytes().starts_with(prefix))
            .flat_map(|(_, folders)| folders)
            .chain(&self.psr0_fallback)
            .find_map(|folder| existing(folder, &psr0_path))
    }
}

/// A value of a map: a string, or an array of keyed values.
enum Value {
    String(Vec<u8>),
    Array(Vec<(Option<Vec<u8>>, Value)>),
}

/// The keyed entries of the array `vendor/composer/<name>` returns, or none when Composer wrote no such map. Each map
/// binds `$vendorDir` and `$baseDir` from `dirname(__DIR__)` before its `return`, which this reads as PHP would.
fn read_map(folder: &Path, name: &str) -> Result<Vec<(Vec<u8>, Value)>, AutoloadError> {
    let map = folder.join(name);
    let contents = match std::fs::read(&map) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AutoloadError::Read { path: map, error }),
    };

    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Owned(map.to_string_lossy().into_owned().into_bytes()), Cow::Owned(contents));
    let program = parse_file(&arena, &file);
    let unsupported =
        |span: Span| AutoloadError::Unsupported { path: map.clone(), line: file.line_number(span.start.offset) + 1 };

    let directory = folder.as_os_str().as_encoded_bytes().to_vec();
    let mut variables: Vec<(&[u8], Vec<u8>)> = Vec::new();
    for statement in program.statements.iter() {
        match statement {
            Statement::Expression(statement) => {
                let Expression::Assignment(assignment) = statement.expression else {
                    return Err(unsupported(statement.expression.span()));
                };
                let Expression::Variable(Variable::Direct(variable)) = assignment.lhs else {
                    return Err(unsupported(assignment.lhs.span()));
                };
                let Value::String(value) = evaluate(assignment.rhs, &variables, &directory).map_err(unsupported)?
                else {
                    return Err(unsupported(assignment.rhs.span()));
                };
                variables.retain(|(name, _)| *name != variable.name);
                variables.push((variable.name, value));
            }
            Statement::Return(r#return) => {
                let Some(value) = r#return.value else {
                    continue;
                };
                let Value::Array(entries) = evaluate(value, &variables, &directory).map_err(unsupported)? else {
                    return Err(unsupported(value.span()));
                };

                return Ok(entries.into_iter().filter_map(|(key, value)| Some((key?, value))).collect());
            }
            _ => {}
        }
    }

    Ok(Vec::new())
}

/// The value of `expression`, built from strings, `.`, `__DIR__`, `dirname()`, the bound string `variables` and
/// arrays, the only code Composer writes in a map, or else the span of the code it holds instead.
fn evaluate(expression: &Expression<'_>, variables: &[(&[u8], Vec<u8>)], directory: &[u8]) -> Result<Value, Span> {
    let string = |expression: &Expression<'_>| match evaluate(expression, variables, directory)? {
        Value::String(string) => Ok(string),
        Value::Array(_) => Err(expression.span()),
    };

    Ok(match expression {
        Expression::Parenthesized(parenthesized) => return evaluate(parenthesized.expression, variables, directory),
        Expression::Literal(Literal::String(literal)) => {
            Value::String(literal.value.ok_or_else(|| expression.span())?.to_vec())
        }
        Expression::MagicConstant(MagicConstant::Directory(_)) => Value::String(directory.to_vec()),
        Expression::Variable(Variable::Direct(variable)) => Value::String(
            variables
                .iter()
                .rev()
                .find(|(name, _)| *name == variable.name)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| expression.span())?,
        ),
        Expression::Binary(binary) if matches!(binary.operator, BinaryOperator::StringConcat(_)) => {
            let mut concatenated = string(binary.lhs)?;
            concatenated.extend(string(binary.rhs)?);
            Value::String(concatenated)
        }
        Expression::Call(Call::Function(call)) if matches!(call.function, Expression::Identifier(name) if name.value().eq_ignore_ascii_case(b"dirname")) =>
        {
            let [Argument::Positional(argument)] = call.argument_list.arguments.as_slice() else {
                return Err(expression.span());
            };
            let path = PathBuf::from(String::from_utf8_lossy(&string(argument.value)?).into_owned());

            Value::String(path.parent().ok_or_else(|| expression.span())?.as_os_str().as_encoded_bytes().to_vec())
        }
        Expression::Array(array) => entries(array.elements.iter(), variables, directory)?,
        Expression::LegacyArray(array) => entries(array.elements.iter(), variables, directory)?,
        _ => return Err(expression.span()),
    })
}

fn entries<'arena>(
    elements: impl Iterator<Item = &'arena ArrayElement<'arena>>,
    variables: &[(&[u8], Vec<u8>)],
    directory: &[u8],
) -> Result<Value, Span> {
    let entries = elements
        .map(|element| match element {
            ArrayElement::KeyValue(element) => {
                let Value::String(key) = evaluate(element.key, variables, directory)? else {
                    return Err(element.key.span());
                };

                Ok((Some(key), evaluate(element.value, variables, directory)?))
            }
            ArrayElement::Value(element) => Ok((None, evaluate(element.value, variables, directory)?)),
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => Err(element.span()),
        })
        .collect::<Result<Vec<_>, Span>>()?;

    Ok(Value::Array(entries))
}

fn path(value: Value) -> PathBuf {
    match value {
        Value::String(path) => PathBuf::from(String::from_utf8_lossy(&path).into_owned()),
        Value::Array(_) => PathBuf::new(),
    }
}

fn paths(value: Value) -> Vec<PathBuf> {
    match value {
        Value::String(_) => vec![path(value)],
        Value::Array(entries) => entries.into_iter().map(|(_, value)| path(value)).collect(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A vendor folder with the maps Composer writes for two packages, one under PSR-4 and one under PSR-0, a class
    /// map entry and a file loaded up front.
    fn vendor() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let header =
            "<?php\n\n// @generated by Composer\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\n";
        write(
            root.path(),
            "vendor/composer/autoload_psr4.php",
            &format!(
                "{header}return array(\n    'Acme\\\\Money\\\\' => array($vendorDir . '/acme/money/src'),\n    'Acme\\\\' => array($vendorDir . '/acme/core/src', $vendorDir . '/acme/extra/src'),\n    'App\\\\' => array($baseDir . '/app'),\n);\n"
            ),
        );
        write(
            root.path(),
            "vendor/composer/autoload_namespaces.php",
            &format!("{header}return array(\n    'Legacy_' => array($vendorDir . '/legacy/lib'),\n);\n"),
        );
        write(
            root.path(),
            "vendor/composer/autoload_classmap.php",
            &format!("{header}return array(\n    'Acme\\\\Mapped' => $vendorDir . '/acme/core/lib/Mapped.php',\n);\n"),
        );
        write(
            root.path(),
            "vendor/composer/autoload_files.php",
            &format!("{header}return array(\n    'a1b2' => $vendorDir . '/acme/core/helpers.php',\n);\n"),
        );
        for file in [
            "vendor/acme/money/src/Money.php",
            "vendor/acme/core/src/Money/Currency.php",
            "vendor/acme/extra/src/Clock.sharp",
            "vendor/legacy/lib/Legacy/Old/Thing.php",
        ] {
            write(root.path(), file, "<?php\n");
        }

        root
    }

    #[test]
    fn a_class_is_found_where_composers_autoloader_finds_it() {
        let root = vendor();
        let vendor = root.path().join("vendor");
        let autoload = Autoload::read(&vendor).unwrap().unwrap();

        assert_eq!(autoload.locate(b"Acme\\Money\\Money"), Some(vendor.join("acme/money/src/Money.php")));
        assert_eq!(autoload.locate(b"\\Acme\\Money\\Currency"), Some(vendor.join("acme/core/src/Money/Currency.php")));
        assert_eq!(autoload.locate(b"Acme\\Clock"), Some(vendor.join("acme/extra/src/Clock.sharp")));
        assert_eq!(autoload.locate(b"Acme\\Mapped"), Some(vendor.join("acme/core/lib/Mapped.php")));
        assert_eq!(autoload.locate(b"Legacy_Old_Thing"), Some(vendor.join("legacy/lib/Legacy/Old/Thing.php")));
        assert_eq!(autoload.locate(b"Acme\\Missing"), None);
        assert_eq!(autoload.files(), [vendor.join("acme/core/helpers.php")]);
    }

    #[test]
    fn the_vendor_folder_is_the_one_composer_json_names_else_vendor() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(vendor_folder(root.path()).unwrap(), root.path().join("vendor"));

        write(root.path(), "composer.json", r#"{"config": {"vendor-dir": "lib/packages"}}"#);
        assert_eq!(vendor_folder(root.path()).unwrap(), root.path().join("lib/packages"));
    }

    #[test]
    fn a_vendor_folder_without_composers_maps_has_no_autoload() {
        let root = tempfile::tempdir().unwrap();

        assert!(Autoload::read(&root.path().join("vendor")).unwrap().is_none());
    }

    #[test]
    fn a_map_holding_other_code_is_refused_with_its_line() {
        let root = vendor();
        write(
            root.path(),
            "vendor/composer/autoload_files.php",
            "<?php\n\nreturn array(\n    'a' => getenv('HOME'),\n);\n",
        );

        let error = Autoload::read(&root.path().join("vendor")).unwrap_err();

        assert!(matches!(error, AutoloadError::Unsupported { line: 4, .. }), "{error}");
    }
}
