use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use foldhash::HashMap;
use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use xxhash_rust::xxh3::Xxh3;

use crate::Location;
use crate::Rules;
use crate::Structure;
use crate::error::StructureError;

const STRUCTURE_PROTOCOL_MAGIC: [u8; 4] = *b"MSTR";
const STRUCTURE_PROTOCOL_MAJOR: u16 = 1;
const STRUCTURE_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;
const STALE_RESPONSE: u16 = 3;

const PACKAGE: [(&str, &str); 4] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Facts.lean", include_str!("../lean/Sharp/Facts.lean")),
    ("Sharp/Runner.lean", include_str!("../lean/Sharp/Runner.lean")),
];
const EXECUTABLE: &str = ".lake/build/bin/sharp-rules";
pub(crate) const LIBRARY: &str = ".lake/build/lib/lean";
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// A fact that breaks a rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub file: String,
    pub location: Location,
    pub rule: String,
    pub module: String,
    pub description: String,
}

/// The `sharp-rules` process. It holds every file's facts and checks every rule over them, and this
/// side holds every file's violations.
#[derive(Debug)]
pub struct Runner {
    pool: WorkerPool,
    generation: u64,
    violations: BTreeMap<String, Vec<Violation>>,
}

impl Runner {
    /// Starts `sharp-rules` on the compiled rules modules.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError`] when `sharp-rules` cannot be started.
    pub fn start(rules: &Rules) -> Result<Self, StructureError> {
        let command = WorkerCommand::new(rules.package().join(EXECUTABLE))
            .with_arguments(rules.modules())
            .with_environment("LEAN_PATH", rules.lean_path()?);

        Ok(Self {
            pool: WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())?,
            generation: 0,
            violations: BTreeMap::new(),
        })
    }

    /// Sends the facts of the `changed` files and the paths of the `removed` ones, and returns every
    /// fact that breaks a rule, sorted. `files` holds every file's encoded facts by path and holds
    /// every changed path. When `sharp-rules` holds another generation, as after a restart, every
    /// file's facts are sent again.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError`] when `sharp-rules` fails or answers with a malformed payload.
    pub fn check(
        &mut self,
        files: &HashMap<String, Vec<u8>>,
        changed: &[&str],
        removed: &[&str],
    ) -> Result<impl Iterator<Item = &Violation>, StructureError> {
        if !self.send(self.generation, files, changed, removed)? {
            let every: Vec<&str> = files.keys().map(String::as_str).collect();
            if !self.send(0, files, &every, &[])? {
                return Err(StructureError::Protocol(
                    "`sharp-rules` refused a request that drops every fact".to_owned(),
                ));
            }
        }

        Ok(self.violations.values().flatten())
    }

    /// Sends one request and applies the answer, or returns `false` when `sharp-rules` holds another
    /// generation than `previous`.
    fn send(
        &mut self,
        previous: u64,
        files: &HashMap<String, Vec<u8>>,
        changed: &[&str],
        removed: &[&str],
    ) -> Result<bool, StructureError> {
        let generation = self.generation + 1;
        let response = self.pool.request(encode_check(previous, generation, files, changed, removed)?)?;
        let Some(answer) = decode_response(&response)? else {
            return Ok(false);
        };

        if previous == 0 {
            self.violations.clear();
        }

        for (file, violations) in answer {
            if violations.is_empty() {
                self.violations.remove(&file);
            } else {
                self.violations.insert(file, violations);
            }
        }

        self.generation = generation;

        Ok(true)
    }
}

impl Structure {
    /// The file's facts as `sharp-rules` reads them: its path, then its declarations, imports and
    /// references, each with its location.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::Protocol`] when a list or string is longer than `u32::MAX`.
    pub fn encode(&self) -> Result<Vec<u8>, StructureError> {
        let mut writer = PayloadWriter::new();
        writer.write_string(&self.file)?;
        writer.write_length(self.declarations.len())?;
        for declaration in &self.declarations {
            writer.write_string(&declaration.name)?;
            writer.write_u8(declaration.kind as u8);
            write_location(&mut writer, &declaration.location);
        }

        writer.write_length(self.imports.len())?;
        for import in &self.imports {
            writer.write_string(&import.namespace)?;
            writer.write_string(&import.name)?;
            writer.write_u8(import.kind as u8);
            write_location(&mut writer, &import.location);
        }

        writer.write_length(self.references.len())?;
        for reference in &self.references {
            writer.write_string(&reference.namespace)?;
            writer.write_string(&reference.name)?;
            write_location(&mut writer, &reference.location);
        }

        Ok(writer.finish())
    }
}

fn write_location(writer: &mut PayloadWriter, location: &Location) {
    writer.write_u32(location.start_line);
    writer.write_u32(location.start_column);
    writer.write_u32(location.end_line);
    writer.write_u32(location.end_column);
}

/// A `Check` request. `previous` is the generation it builds on, and `0` drops every fact first.
fn encode_check(
    previous: u64,
    generation: u64,
    files: &HashMap<String, Vec<u8>>,
    changed: &[&str],
    removed: &[&str],
) -> Result<Vec<u8>, StructureError> {
    let mut writer = PayloadWriter::with_capacity(64 + changed.iter().map(|path| files[*path].len()).sum::<usize>());
    writer.write_raw(&STRUCTURE_PROTOCOL_MAGIC);
    writer.write_u16(STRUCTURE_PROTOCOL_MAJOR);
    writer.write_u16(STRUCTURE_PROTOCOL_MINOR);
    writer.write_u16(CHECK_REQUEST);
    writer.write_u16(0);
    writer.write_u64(previous);
    writer.write_u64(generation);
    writer.write_length(removed.len())?;
    for path in removed {
        writer.write_string(path)?;
    }

    writer.write_length(changed.len())?;
    for path in changed {
        writer.write_raw(&files[*path]);
    }

    Ok(writer.finish())
}

/// Every file whose violations may have changed, with all of its violations, sorted.
type Answer = Vec<(String, Vec<Violation>)>;

/// The answer to a request, or `None` for a `Stale` answer: `sharp-rules` holds another generation
/// than the request built on, and applied nothing.
fn decode_response(payload: &[u8]) -> Result<Option<Answer>, StructureError> {
    let mut reader = PayloadReader::new(payload);
    if reader.read_array::<4>("structure message magic")? != STRUCTURE_PROTOCOL_MAGIC {
        return Err(StructureError::Protocol("the answer does not start with MSTR".to_owned()));
    }

    let major = reader.read_u16("structure protocol major version")?;
    let minor = reader.read_u16("structure protocol minor version")?;
    if major != STRUCTURE_PROTOCOL_MAJOR {
        return Err(StructureError::Protocol(format!("unsupported structure protocol version {major}.{minor}")));
    }

    let kind = reader.read_u16("structure message kind")?;
    reader.read_u16("structure message reserved header")?;
    reader.read_u64("generation")?;
    let files = match kind {
        STALE_RESPONSE => None,
        CHECKED_RESPONSE => {
            let file_count = reader.read_count("file count", reader.remaining())?;
            let mut files = Vec::with_capacity(file_count);
            for _ in 0..file_count {
                let file = reader.read_string("violation file")?;
                let count = reader.read_count("violation count", reader.remaining())?;
                let mut violations = Vec::with_capacity(count);
                for _ in 0..count {
                    violations.push(Violation {
                        file: file.clone(),
                        rule: reader.read_string("rule")?,
                        module: reader.read_string("rule module")?,
                        location: Location {
                            start_line: reader.read_u32("start line")?,
                            start_column: reader.read_u32("start column")?,
                            end_line: reader.read_u32("end line")?,
                            end_column: reader.read_u32("end column")?,
                        },
                        description: reader.read_string("violation description")?,
                    });
                }

                violations.sort();
                files.push((file, violations));
            }

            Some(files)
        }
        kind => return Err(StructureError::Protocol(format!("unknown structure message kind {kind}"))),
    };

    reader.finish()?;

    Ok(files)
}

/// Writes the Lean package of `sharp-rules` to `structure/<key>` in Mago's cache folder, the key a
/// hash of its sources, and builds it with Lake once per key. Builds in one process take turns,
/// builds in different processes run in their own directories, and the first one renamed into place
/// wins. A new key deletes every older one.
pub(crate) fn build_package() -> Result<PathBuf, StructureError> {
    static BUILD: Mutex<()> = Mutex::new(());

    let mut key = Xxh3::new();
    for (path, source) in PACKAGE {
        key.update(path.as_bytes());
        key.update(source.as_bytes());
    }

    let packages = cache_root()?.join("structure");
    let package = packages.join(format!("{:016x}", key.digest()));
    let _build = BUILD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if package.join(EXECUTABLE).is_file() {
        return Ok(package);
    }

    let staging = package.with_extension(std::process::id().to_string());
    for (path, source) in PACKAGE {
        let target = staging.join(path);
        fs::create_dir_all(target.parent().unwrap_or(&staging))?;
        fs::write(target, source)?;
    }

    let output =
        Command::new("lake").arg("build").current_dir(&staging).output().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => StructureError::MissingLean(error),
            _ => StructureError::IoError(error),
        })?;
    if !output.status.success() {
        return Err(StructureError::LeanFailed {
            file: staging.join("lakefile.toml"),
            output: format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)),
        });
    }

    if fs::rename(&staging, &package).is_err() {
        fs::remove_dir_all(&staging)?;
    }

    for entry in fs::read_dir(&packages)? {
        let old = entry?.path();
        let built = old.extension().is_none() && old.is_dir();
        if built && old != package {
            fs::remove_dir_all(old)?;
        }
    }

    Ok(package)
}

/// Mago's cache folder, as the warm analysis server's: `$XDG_CACHE_HOME/mago`, else
/// `~/Library/Caches/mago` on macOS and `~/.cache/mago` elsewhere.
fn cache_root() -> Result<PathBuf, StructureError> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|cache| !cache.is_empty()) {
        return Ok(PathBuf::from(cache).join("mago"));
    }

    let home = std::env::home_dir().ok_or_else(|| io::Error::other("Mago's cache folder needs a home folder"))?;

    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/mago") } else { home.join(".cache/mago") })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::Import;
    use crate::SymbolKind;

    const RULES: &str = "import Sharp.Facts\nopen Sharp\nnamespace App\n\
        @[rule] def sharedNeverImportsTenant : Rule := .neverImports \"App.Shared\" \"App.Tenant\"\n\
        @[rule] def noImportCycles : Rule := .noImportCycles\nend App\n";

    fn rules() -> (tempfile::TempDir, Rules) {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir_all(workspace.path().join("src/App")).unwrap();
        fs::write(workspace.path().join("src/App.lean"), RULES).unwrap();
        fs::write(workspace.path().join("composer.json"), r#"{"autoload": {"psr-4": {"App\\": "src/App/"}}}"#).unwrap();
        let (rules, issues) = Rules::compile(workspace.path(), &["src".to_owned()]).unwrap();
        assert!(issues.is_empty(), "{issues:?}");

        (workspace, rules)
    }

    fn file(path: &str, namespace: &str, imported: &str) -> (String, Vec<u8>) {
        let import = Import {
            namespace: namespace.to_owned(),
            name: imported.to_owned(),
            kind: SymbolKind::ClassLike,
            location: Location { start_line: 3, start_column: 5, end_line: 3, end_column: 9 },
        };
        let structure = Structure { file: path.to_owned(), imports: vec![import], ..Structure::default() };

        (path.to_owned(), structure.encode().unwrap())
    }

    fn broken(violations: &[Violation]) -> Vec<(&str, &str)> {
        violations.iter().map(|violation| (violation.rule.as_str(), violation.file.as_str())).collect()
    }

    fn check(
        runner: &mut Runner,
        files: &HashMap<String, Vec<u8>>,
        changed: &[&str],
        removed: &[&str],
    ) -> Vec<Violation> {
        runner.check(files, changed, removed).unwrap().cloned().collect()
    }

    fn full_check(rules: &Rules, files: &HashMap<String, Vec<u8>>) -> Vec<Violation> {
        let every: Vec<&str> = files.keys().map(String::as_str).collect();

        check(&mut Runner::start(rules).unwrap(), files, &every, &[])
    }

    #[test]
    fn rechecks_changed_and_removed_files_as_a_full_check_does() {
        let (_workspace, rules) = rules();
        let mut files: HashMap<String, Vec<u8>> =
            [file("a.php", "App.Shared", "App.Tenant.Store"), file("b.php", "App.Tenant", "App.Shared.Money")]
                .into_iter()
                .collect();
        let mut runner = Runner::start(&rules).unwrap();

        let violations = check(&mut runner, &files, &["a.php", "b.php"], &[]);
        assert_eq!(
            broken(&violations),
            [
                ("App.noImportCycles", "a.php"),
                ("App.sharedNeverImportsTenant", "a.php"),
                ("App.noImportCycles", "b.php")
            ]
        );
        assert_eq!(violations[1].description, "imports App.Tenant.Store");
        assert_eq!(violations[1].module, "App");
        assert_eq!(violations[1].location, Location { start_line: 3, start_column: 5, end_line: 3, end_column: 9 });

        files.extend([file("b.php", "App.Tenant", "Lib.Money")]);
        let violations = check(&mut runner, &files, &["b.php"], &[]);
        assert_eq!(broken(&violations), [("App.sharedNeverImportsTenant", "a.php")]);
        assert_eq!(violations, full_check(&rules, &files));

        files.remove("a.php");
        let violations = check(&mut runner, &files, &[], &["a.php"]);
        assert_eq!(violations, []);
        assert_eq!(violations, full_check(&rules, &files));
    }

    #[test]
    fn sends_every_file_again_when_the_runner_holds_another_generation() {
        let (_workspace, rules) = rules();
        let files: HashMap<String, Vec<u8>> =
            std::iter::once(file("a.php", "App.Shared", "App.Tenant.Store")).collect();
        let mut runner = Runner::start(&rules).unwrap();
        check(&mut runner, &files, &["a.php"], &[]);

        assert!(!runner.send(42, &files, &[], &[]).unwrap());

        runner.generation = 42;
        let violations = check(&mut runner, &files, &[], &[]);
        assert_eq!(broken(&violations), [("App.sharedNeverImportsTenant", "a.php")]);
    }
}
