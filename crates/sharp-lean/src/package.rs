//! `.sharp/.lean/`, the Lake package that builds the generated modules and the proof files.

use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::str::FromStr;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_analyzer::graph::strongly_connected_parts;
use mago_composer::AutoloadPsr4value;
use mago_composer::ComposerPackage;
use mago_composer::ComposerPackageAutoloadDevPsr4value;
use mago_database::file::FileId;
use mago_sharp_bridge::unit::COMPILED_FOLDER;
use serde::Deserialize;
use serde::Serialize;

use crate::issues;
use crate::library;
use crate::translate::Declaration;
use crate::translate::Translation;
use crate::translate::Unmodeled;
use crate::translate::Use;

/// The folder in the `.sharp` folder that holds the Lake package of the project's laws.
pub const PACKAGE_FOLDER: &str = ".lean";

/// Every declaration of every translation, indexed by its Lean name.
pub(crate) struct Program<'program> {
    translations: &'program [Translation],
    declarations: HashMap<&'program str, (usize, &'program Declaration)>,
    /// The declarations in a cycle of uses: a recursion, of methods or of types.
    recursive: HashMap<&'program str, Unmodeled>,
}

/// Why a declaration cannot be written to Lean.
#[derive(Default)]
pub(crate) struct Problems {
    pub(crate) unmodeled: Vec<Unmodeled>,
    /// The uses whose declaration no translation holds.
    pub(crate) missing: Vec<Use>,
}

/// A generated module and the module of its group that holds the group's declarations: itself, or the one it imports.
pub(crate) struct Module {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) text: String,
}

impl Problems {
    pub(crate) fn is_empty(&self) -> bool {
        self.unmodeled.is_empty() && self.missing.is_empty()
    }
}

impl<'program> Program<'program> {
    pub(crate) fn new(translations: &'program [Translation]) -> Self {
        let mut declarations = HashMap::default();
        for (index, translation) in translations.iter().enumerate() {
            for declaration in &translation.declarations {
                declarations.insert(declaration.key.as_str(), (index, declaration));
            }
        }

        let keys: Vec<&str> = declarations.keys().copied().collect();
        let positions: HashMap<&str, usize> = keys.iter().enumerate().map(|(position, key)| (*key, position)).collect();
        let successors: Vec<Vec<usize>> = keys
            .iter()
            .map(|key| {
                declarations[key].1.uses.iter().filter_map(|used| positions.get(used.key.as_str()).copied()).collect()
            })
            .collect();
        let mut recursive = HashMap::default();
        for part in strongly_connected_parts(&successors) {
            let cyclic = part.len() > 1 || part.iter().any(|node| successors[*node].contains(node));
            if !cyclic {
                continue;
            }
            for node in part {
                let declaration = declarations[keys[node]].1;
                recursive.insert(
                    keys[node],
                    Unmodeled {
                        span: declaration.span,
                        what: "a recursion".to_owned(),
                        place: place_of(keys[node]),
                        reason: issues::RECURSION,
                    },
                );
            }
        }

        Self { translations, declarations, recursive }
    }

    /// Every declaration `key` uses, transitively, `key`'s own included.
    fn closure(&self, key: &str) -> (Vec<&'program str>, Vec<Use>) {
        let mut reached = Vec::new();
        let mut missing: Vec<Use> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::default();
        let mut pending = vec![key];
        while let Some(next) = pending.pop() {
            let Some((&key, (_, declaration))) = self.declarations.get_key_value(next) else {
                continue;
            };
            if !seen.insert(key) {
                continue;
            }
            reached.push(key);
            for used in &declaration.uses {
                if self.declarations.contains_key(used.key.as_str()) {
                    pending.push(used.key.as_str());
                } else if !missing.iter().any(|known| known.key == used.key) {
                    missing.push(used.clone());
                }
            }
        }

        (reached, missing)
    }

    /// Why the declaration `key`, or one it uses, cannot be written to Lean.
    pub(crate) fn problems(&self, key: &str) -> Problems {
        let (reached, missing) = self.closure(key);
        let mut problems = Problems { unmodeled: Vec::new(), missing };
        for reached in reached {
            let declaration = self.declarations[reached].1;
            for construct in declaration.unmodeled.iter().chain(self.recursive.get(reached)) {
                if !problems.unmodeled.iter().any(|known| known.span == construct.span && known.what == construct.what)
                {
                    problems.unmodeled.push(construct.clone());
                }
            }
        }

        problems
    }

    /// The definitions a proof of the law `statement` unfolds besides the law itself.
    pub(crate) fn definitions(&self, statement: &str) -> Vec<&'program str> {
        let (mut reached, _) = self.closure(statement);
        reached.retain(|key| *key != statement && self.declarations[key].1.unfolds);
        reached.sort_unstable();

        reached
    }

    /// The generated module of each translation that names one and declares something or is in `proved`, whose proof
    /// file imports it. A declaration that cannot be written to Lean is left out. Files whose declarations use each
    /// other form one group, whose declarations all live in the module that sorts first; each other module of the
    /// group imports it, so no two modules import each other, as Lean requires.
    pub(crate) fn modules(&self, proved: &[FileId]) -> Vec<Module> {
        let sound: HashSet<&str> =
            self.declarations.keys().copied().filter(|key| self.problems(key).is_empty()).collect();

        let named: Vec<usize> = (0..self.translations.len())
            .filter(|index| {
                let translation = &self.translations[*index];
                translation.class.is_some()
                    && (!translation.declarations.is_empty() || proved.contains(&translation.file))
            })
            .collect();
        let position: HashMap<usize, usize> =
            named.iter().enumerate().map(|(position, index)| (*index, position)).collect();
        let successors: Vec<Vec<usize>> = named
            .iter()
            .map(|index| {
                let mut successors: Vec<usize> = self.translations[*index]
                    .declarations
                    .iter()
                    .filter(|declaration| sound.contains(declaration.key.as_str()))
                    .flat_map(|declaration| &declaration.uses)
                    .filter_map(|used| self.declarations.get(used.key.as_str()))
                    .filter_map(|(used, _)| position.get(used).copied())
                    .filter(|used| *used != position[index])
                    .collect();
                successors.sort_unstable();
                successors.dedup();
                successors
            })
            .collect();

        let mut modules = Vec::new();
        let mut host_of: HashMap<usize, String> = HashMap::default();
        for part in strongly_connected_parts(&successors) {
            let mut members: Vec<(String, usize)> =
                part.iter().map(|position| (module_name(&self.translations[named[*position]]), *position)).collect();
            members.sort();
            let host = members[0].0.clone();
            for (_, position) in &members {
                host_of.insert(*position, host.clone());
            }

            let mut imports: Vec<String> = members
                .iter()
                .flat_map(|(_, position)| &successors[*position])
                .filter(|used| !part.contains(used))
                .map(|used| host_of[used].clone())
                .collect();
            imports.sort();
            imports.dedup();

            let mut text = String::from(
                "-- Generated by mago compile. Do not edit.\nimport Sharp.Lean.Law\nimport Sharp.Lean.String\n",
            );
            for import in &imports {
                let _ = writeln!(text, "import {import}");
            }
            text.push_str("open Sharp\n");
            for key in self.ordered(members.iter().map(|(_, position)| named[*position]), &sound) {
                text.push('\n');
                text.push_str(&self.declarations[key].1.text);
                text.push('\n');
            }
            for (name, _) in &members[1..] {
                modules.push(Module {
                    name: name.clone(),
                    host: host.clone(),
                    text: format!("-- Generated by mago compile. Do not edit.\nimport {host}\n"),
                });
            }
            modules.push(Module { name: host.clone(), host, text });
        }

        modules
    }

    /// The sound declarations of the translations at `indexes`, each after every declaration it uses.
    fn ordered(&self, indexes: impl Iterator<Item = usize>, sound: &HashSet<&str>) -> Vec<&'program str> {
        let mut keys: Vec<&str> = indexes
            .flat_map(|index| &self.translations[index].declarations)
            .map(|declaration| declaration.key.as_str())
            .filter(|key| sound.contains(key))
            .collect();
        keys.sort_unstable();
        let position: HashMap<&str, usize> = keys.iter().enumerate().map(|(position, key)| (*key, position)).collect();
        let successors: Vec<Vec<usize>> = keys
            .iter()
            .map(|key| {
                self.declarations[key]
                    .1
                    .uses
                    .iter()
                    .filter_map(|used| position.get(used.key.as_str()).copied())
                    .collect()
            })
            .collect();

        strongly_connected_parts(&successors).into_iter().flatten().map(|position| keys[position]).collect()
    }
}

/// The generated module of `translation`: `Code.` and its class's Lean name.
pub(crate) fn module_name(translation: &Translation) -> String {
    format!("Code.{}", translation.class.as_ref().map_or("", |(class, _)| class.as_str()))
}

/// The source file of `module` in the package, relative to it: `Code/App/Shared/Money.lean` of
/// `Code.App.Shared.Money`, and `Proofs/App/Shared/Money.lean` of the proof module `App.Shared.Money`.
pub(crate) fn source_of(module: &str) -> PathBuf {
    let path = PathBuf::from(module.replace('.', "/")).with_extension("lean");

    if module.starts_with("Code.") { path } else { Path::new("Proofs").join(path) }
}

/// `Money.add` of `App.Shared.Money.add`.
fn place_of(key: &str) -> String {
    let mut parts = key.rsplitn(3, '.');
    match (parts.next(), parts.next()) {
        (Some(member), Some(class)) => format!("{class}.{member}"),
        (Some(name), None) => name.to_owned(),
        _ => key.to_owned(),
    }
}

/// A PSR-4 folder and the PHP namespace it holds, dot-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Root {
    pub(crate) namespace: String,
    pub(crate) directory: PathBuf,
}

/// Every PSR-4 folder of the `composer.json` in `package`, in `autoload` and `autoload-dev`, read as `mago init` reads
/// them. No `composer.json`, no folders.
pub(crate) fn roots(package: &Path) -> io::Result<Vec<Root>> {
    let json = match fs::read_to_string(package.join("composer.json")) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let composer = ComposerPackage::from_str(&json).map_err(io::Error::other)?;
    let mut prefixes: Vec<(&String, Vec<String>)> = Vec::new();
    if let Some(autoload) = &composer.autoload {
        prefixes.extend(autoload.psr_4.iter().map(|(prefix, directories)| match directories {
            AutoloadPsr4value::Array(directories) => (prefix, directories.clone()),
            AutoloadPsr4value::String(directory) => (prefix, vec![directory.clone()]),
        }));
    }
    if let Some(autoload_dev) = &composer.autoload_dev {
        prefixes.extend(autoload_dev.psr_4.iter().map(|(prefix, directories)| match directories {
            ComposerPackageAutoloadDevPsr4value::Array(directories) => (prefix, directories.clone()),
            ComposerPackageAutoloadDevPsr4value::String(directory) => (prefix, vec![directory.clone()]),
        }));
    }

    Ok(prefixes
        .into_iter()
        .flat_map(|(prefix, directories)| {
            let namespace = prefix.trim_matches('\\').replace('\\', ".");
            directories
                .into_iter()
                .map(move |directory| Root { namespace: namespace.clone(), directory: package.join(directory) })
        })
        .collect())
}

/// The Lean module of the proof file `file`: the PHP namespace of its folder, under the PSR-4 folder that holds it most
/// closely, and its name. `None` when no PSR-4 folder with a namespace holds it.
pub(crate) fn proof_module(roots: &[Root], file: &Path) -> Option<String> {
    let (root, relative) = roots
        .iter()
        .filter(|root| !root.namespace.is_empty())
        .filter_map(|root| Some((root, file.with_extension("").strip_prefix(&root.directory).ok()?.to_path_buf())))
        .min_by_key(|(_, relative)| relative.components().count())?;

    let segments: Vec<String> = std::iter::once(root.namespace.clone())
        .chain(relative.components().map(|component| component.as_os_str().to_string_lossy().into_owned()))
        .collect();

    Some(segments.join("."))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Lakefile<'lakefile> {
    name: &'lakefile str,
    require: [Require<'lakefile>; 1],
    #[serde(rename = "lean_lib")]
    lean_lib: Vec<LeanLib<'lakefile>>,
}

#[derive(Serialize)]
struct Require<'lakefile> {
    name: &'lakefile str,
    path: &'lakefile Path,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LeanLib<'lakefile> {
    name: &'lakefile str,
    #[serde(skip_serializing_if = "Option::is_none")]
    src_dir: Option<&'lakefile str>,
    globs: Vec<String>,
}

/// One message of `lean --json`.
#[derive(Debug, Deserialize)]
pub(crate) struct Message {
    pub(crate) severity: String,
    pub(crate) data: String,
    pub(crate) pos: Position,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Position {
    pub(crate) line: u32,
}

/// What `lake` answered when asked its version.
pub(crate) enum Toolchain {
    Ready,
    /// `lake` is not on the `PATH`.
    Missing,
    /// elan could not install the pinned Lean version, with its message.
    Failed(String),
}

/// The package in `.sharp/.lean/` under a workspace root.
pub(crate) struct Package {
    pub(crate) directory: PathBuf,
}

impl Package {
    /// The package of the workspace at `root`, in `.sharp/.lean/` apart from the `.sharpc` mirror of the sources. The
    /// folder is joined one name at a time, since `cmd` reads a `/` in a Windows path as a switch.
    pub(crate) fn new(root: &Path) -> Self {
        Self { directory: plain(root).join(COMPILED_FOLDER).join(PACKAGE_FOLDER) }
    }

    /// Writes the package's Lean version, so elan installs it on first use, and asks `lake` for its version there.
    pub(crate) fn toolchain(&self) -> io::Result<Toolchain> {
        fs::create_dir_all(&self.directory)?;
        write_if_changed(&self.directory.join("lean-toolchain"), library::TOOLCHAIN.as_bytes())?;

        match Command::new("lake").arg("--version").current_dir(&self.directory).output() {
            Ok(output) if output.status.success() => Ok(Toolchain::Ready),
            Ok(output) => Ok(Toolchain::Failed(
                format!("{}{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout))
                    .trim()
                    .to_owned(),
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Toolchain::Missing),
            Err(error) => Err(error),
        }
    }

    /// Writes `lakefile.toml`, which requires the runtime library by path, each generated module whose bytes changed,
    /// and one link in `Proofs/` per PSR-4 root, then deletes every generated module no translation names any more.
    /// A changed `lakefile.toml` drops Lake's manifest, so Lake resolves the library again.
    pub(crate) fn write(&self, library: &Path, roots: &[Root], modules: &[Module]) -> io::Result<()> {
        let linked = linked_roots(roots);
        let mut lean_lib = vec![LeanLib { name: "Code", src_dir: None, globs: vec!["Code.+".to_owned()] }];
        if !linked.is_empty() {
            lean_lib.push(LeanLib {
                name: "Proofs",
                src_dir: Some("Proofs"),
                globs: linked.iter().map(|root| format!("{}.+", root.namespace)).collect(),
            });
        }
        let lakefile = toml::to_string(&Lakefile {
            name: "proofs",
            require: [Require { name: "sharp", path: library }],
            lean_lib,
        })
        .map_err(io::Error::other)?;
        if write_if_changed(&self.directory.join("lakefile.toml"), lakefile.as_bytes())? {
            match fs::remove_file(self.directory.join("lake-manifest.json")) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }

        let code = self.directory.join("Code");
        let mut written: HashSet<PathBuf> = HashSet::default();
        for module in modules {
            let path = self.directory.join(source_of(&module.name));
            write_if_changed(&path, module.text.as_bytes())?;
            written.insert(path);
        }
        if code.is_dir() {
            delete_all_but(&code, &written)?;
        }

        let proofs = self.directory.join("Proofs");
        if proofs.exists() {
            fs::remove_dir_all(&proofs)?;
        }
        for root in linked {
            let link = root.namespace.split('.').fold(proofs.clone(), |link, name| link.join(name));
            fs::create_dir_all(link.parent().unwrap_or(&proofs))?;
            link_directory(&root.directory, &link)?;
        }

        Ok(())
    }

    /// Builds `modules` with Lake, and returns the ones that did not build. One build runs them all; when it fails, each
    /// module builds again on its own, which replays every module the first build finished, to tell them apart.
    pub(crate) fn build(&self, modules: &[String]) -> io::Result<Vec<String>> {
        if modules.is_empty() || self.lake_build(modules)?.is_ok() {
            return Ok(Vec::new());
        }

        let mut failed = Vec::new();
        for module in modules {
            if self.lake_build(std::slice::from_ref(module))?.is_err() {
                failed.push(module.clone());
            }
        }

        Ok(failed)
    }

    fn lake_build(&self, modules: &[String]) -> io::Result<Result<(), String>> {
        let output = Command::new("lake").arg("build").args(modules).current_dir(&self.directory).output()?;

        Ok(if output.status.success() {
            Ok(())
        } else {
            Err(format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
        })
    }

    /// The errors Lean reports for the package's source file `source`, with each error's line, from
    /// `lake env lean --json`: Lake's own log keeps a message's position only inside its text.
    pub(crate) fn errors(&self, source: &Path) -> io::Result<Vec<Message>> {
        let output =
            Command::new("lake").args(["env", "lean", "--json"]).arg(source).current_dir(&self.directory).output()?;

        let mut errors = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines().filter(|line| line.starts_with('{')) {
            let message: Message = serde_json::from_str(line).map_err(io::Error::other)?;
            if message.severity == "error" {
                errors.push(message);
            }
        }
        if errors.is_empty() && !output.status.success() {
            errors.push(Message {
                severity: "error".to_owned(),
                data: format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
                    .trim()
                    .to_owned(),
                pos: Position { line: 1 },
            });
        }

        Ok(errors)
    }
}

/// The roots that get a link: each with a namespace, apart from one whose link would sit inside another's, since that
/// link would be written into the linked folder of the project.
fn linked_roots(roots: &[Root]) -> Vec<&Root> {
    let mut linked: Vec<&Root> =
        roots.iter().filter(|root| !root.namespace.is_empty() && root.directory.is_dir()).collect();
    linked.sort_by(|a, b| a.namespace.cmp(&b.namespace));
    linked.dedup_by(|a, b| a.namespace == b.namespace);

    let namespaces: Vec<String> = linked.iter().map(|root| root.namespace.clone()).collect();
    linked.retain(|root| {
        !namespaces
            .iter()
            .any(|other| root.namespace.len() > other.len() && root.namespace.starts_with(&format!("{other}.")))
    });

    linked
}

/// Writes `bytes` to `path` by rename unless the file already holds them, so Lake keeps the trace of an unchanged
/// module. Returns whether it wrote.
fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }

    let folder = path.parent().unwrap_or(path);
    fs::create_dir_all(folder)?;
    let file = tempfile::NamedTempFile::new_in(folder)?;
    fs::write(file.path(), bytes)?;
    let (file, written) = file.keep().map_err(|error| error.error)?;
    drop(file);
    fs::rename(&written, path).inspect_err(|_| {
        let _ = fs::remove_file(&written);
    })?;

    Ok(true)
}

/// Deletes every `.lean` file under `folder` that is not in `kept`, then every folder left empty, `folder` included.
fn delete_all_but(folder: &Path, kept: &HashSet<PathBuf>) -> io::Result<bool> {
    let mut empty = true;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            empty &= delete_all_but(&path, kept)?;
        } else if kept.contains(&path) {
            empty = false;
        } else {
            fs::remove_file(&path)?;
        }
    }
    if empty {
        fs::remove_dir(folder)?;
    }

    Ok(empty)
}

/// `path` without the `\\?\` prefix `canonicalize` gives a Windows path, which neither `cmd` nor Lake reads.
fn plain(path: &Path) -> PathBuf {
    match path.to_str().and_then(|path| path.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(unix)]
fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// A directory junction, which Windows lets every user create, unlike a symbolic link.
#[cfg(windows)]
fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
    let output = Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(plain(target)).output()?;
    if output.status.success() {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "cannot link {} to {}: {}",
        link.display(),
        target.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_module_is_named_after_the_psr_4_namespace_of_its_folder() {
        let roots = [
            Root { namespace: "App".to_owned(), directory: PathBuf::from("/w/app/") },
            Root { namespace: "App.Tenant.Billing".to_owned(), directory: PathBuf::from("/w/modules/billing") },
            Root { namespace: String::new(), directory: PathBuf::from("/w/lib") },
        ];

        assert_eq!(proof_module(&roots, Path::new("/w/app/Shared/Money.lean")).as_deref(), Some("App.Shared.Money"));
        assert_eq!(
            proof_module(&roots, Path::new("/w/modules/billing/Invoice.lean")).as_deref(),
            Some("App.Tenant.Billing.Invoice")
        );
        assert_eq!(proof_module(&roots, Path::new("/w/lib/Vendor.lean")), None);
        assert_eq!(proof_module(&roots, Path::new("/w/other/Thing.lean")), None);
    }

    #[test]
    fn a_root_whose_link_would_sit_inside_another_link_gets_none() {
        let roots = [
            Root { namespace: "App".to_owned(), directory: std::env::temp_dir() },
            Root { namespace: "App.Tenant".to_owned(), directory: std::env::temp_dir() },
            Root { namespace: "Lib".to_owned(), directory: std::env::temp_dir() },
        ];

        let linked: Vec<&str> = linked_roots(&roots).iter().map(|root| root.namespace.as_str()).collect();

        assert_eq!(linked, ["App", "Lib"]);
    }

    #[test]
    fn a_verbatim_windows_path_loses_its_prefix_and_any_other_path_is_kept() {
        assert_eq!(plain(Path::new(r"\\?\C:\work\app")), PathBuf::from(r"C:\work\app"));
        assert_eq!(plain(Path::new(r"\\?\UNC\server\share")), PathBuf::from(r"\\?\UNC\server\share"));
        assert_eq!(plain(Path::new("/work/app")), PathBuf::from("/work/app"));
    }

    #[test]
    fn a_member_is_placed_by_its_class_and_name() {
        assert_eq!(place_of("App.Shared.Money.add"), "Money.add");
    }
}
