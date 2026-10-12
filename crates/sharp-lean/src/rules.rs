//! Structure rules: each `@[rule]` of a `Module.lean` constrains the namespace of its folder, and judges the facts of
//! every `.sharp` file under that namespace.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_database::file::FileId;
use mago_reporting::IssueCollection;
use mago_sharp_bridge::unit;

use crate::facts::Facts;
use crate::issues;
use crate::lean::VENDOR;
use crate::lean::proof_file;
use crate::package;
use crate::package::Message;
use crate::package::Package;
use crate::package::Root;
use crate::runner;
use crate::translate::Translation;

/// A Lean file of the workspace that the structure rules step builds.
#[derive(Debug)]
pub(crate) struct LeanFile {
    /// Workspace-relative, with `/` separators: `app/Shared/Module.lean`.
    pub(crate) path: String,
    /// Its Lean module, as the PSR-4 folder that holds it names it: `App.Shared.Module`.
    pub(crate) module: String,
    /// The namespace of its folder: `App.Shared`. The rules of a `Module.lean` constrain it.
    pub(crate) namespace: String,
    pub(crate) role: Role,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// A `Module.lean`, which holds the structure rules of its namespace.
    Rules,
    /// A Lean file a `Module.lean` imports, directly or through other Lean files. Lean's error in it is reported on the
    /// files of each namespace whose `Module.lean` imports it.
    Imported,
    /// A Lean file in a PSR-4 folder that is neither a `Module.lean` nor the proof file of a `.sharp` file, and that no
    /// Lean file imports. It is built so that a `@[rule]` in it is an error.
    Unreached,
}

/// A Lean file of the workspace that cannot be built, which refuses each `.sharp` file under its folder.
#[derive(Debug)]
pub(crate) enum Unbuilt {
    /// A `Module.lean` in no PSR-4 folder, whose rules have no Lean module name.
    Unnamed(String),
    /// A Lean file, other than a proof file, in a PSR-4 folder under a root Lean or Mago owns, with the namespace of
    /// that folder.
    Owned(String, String),
}

/// The files whose structure rules may judge the workspace-relative file `name`.
///
/// They are the `Module.lean` of each folder that holds it, up to the workspace `root`, and each workspace Lean file
/// they import, transitively. An import names a workspace file through the PSR-4 folders of the workspace's
/// `composer.json`, which is read only when one of those files imports a module.
///
/// # Errors
///
/// Returns an error when one of those files or `composer.json` exists and cannot be read.
pub fn rules_files(root: &Path, name: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut files: Vec<Vec<u8>> = std::iter::once(&name[..0])
        .chain(name.iter().enumerate().filter(|(_, byte)| **byte == b'/').map(|(end, _)| &name[..=end]))
        .map(|folder| [folder, b"Module.lean"].concat())
        .collect();
    let mut roots: Option<Vec<Root>> = None;
    let mut next = 0;
    while next < files.len() {
        let text = match fs::read_to_string(root.join(String::from_utf8_lossy(&files[next]).as_ref())) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        next += 1;
        for (_, import) in imports(&text) {
            if roots.is_none() {
                roots = Some(package::roots(root)?);
            }
            if let Some(file) = workspace_file(root, roots.as_deref().unwrap_or_default(), import)
                && !files.contains(&file)
            {
                files.push(file);
            }
        }
    }

    Ok(files)
}

/// Every Lean file of the workspace the structure rules step builds, in path order: each of the `rules_files` of the
/// `.sharp` files of `translations` outside `vendor/`, and each unreached Lean file in a PSR-4 folder of `roots`. Also
/// returns each Lean file that cannot be built: each `Module.lean` of those `rules_files` that no PSR-4 folder names a
/// module for, and each Lean file other than a proof file in a PSR-4 folder under a root Lean or Mago owns.
pub(crate) fn find(
    root: &Path,
    roots: &[Root],
    translations: &[Translation],
) -> io::Result<(Vec<LeanFile>, Vec<Unbuilt>)> {
    let mut paths = BTreeSet::new();
    for translation in translations.iter().filter(|translation| !translation.name.starts_with(VENDOR)) {
        for path in rules_files(root, &translation.name)? {
            paths.insert(String::from_utf8_lossy(&path).into_owned());
        }
    }

    let mut found = Vec::new();
    let mut unbuilt = Vec::new();
    for path in paths {
        let file = root.join(&path);
        if !file.is_file() || package::owned_root(roots, &file).is_some() {
            continue;
        }
        let role = if is_module_lean(&path) { Role::Rules } else { Role::Imported };
        match package::proof_module(roots, &file) {
            Some(module) => found.push(lean_file(path, module, role)),
            None if role == Role::Rules => unbuilt.push(Unbuilt::Unnamed(path)),
            None => {}
        }
    }

    let vendor = root.join("vendor");
    let mut walked = Vec::new();
    for linked in package::linked_roots(roots) {
        walk(&linked.directory, &vendor, &mut walked)?;
    }
    let mut imported = HashSet::default();
    for file in &walked {
        imported.extend(imports(&fs::read_to_string(file)?).map(|(_, import)| import.to_owned()));
    }
    for file in walked {
        let Ok(relative) = file.strip_prefix(root) else {
            continue;
        };
        let path = slashed(relative);
        let Some(module) = package::proof_module(roots, &file) else {
            continue;
        };
        if is_module_lean(&path)
            || file.with_extension("sharp").is_file()
            || imported.contains(&module)
            || package::owned_root(roots, &file).is_some()
            || found.iter().any(|found| found.path == path)
        {
            continue;
        }
        found.push(lean_file(path, module, Role::Unreached));
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));

    let mut owned = Vec::new();
    for folder in package::owned_roots(roots).filter(|owned| owned.directory.is_dir()) {
        walk(&folder.directory, &vendor, &mut owned)?;
    }
    owned.sort();
    owned.dedup();
    for file in owned {
        if let (Ok(relative), Some(folder)) = (file.strip_prefix(root), package::owned_root(roots, &file))
            && !file.with_extension("sharp").is_file()
        {
            unbuilt.push(Unbuilt::Owned(slashed(relative), folder.namespace.clone()));
        }
    }

    Ok((found, unbuilt))
}

/// Each of `files` with the hash of its source, one per line. The structure rules check reads them, and an unreached
/// Lean file is the input of no `.sharpc`, so the check runs again whenever they differ from what the last check read.
pub(crate) fn sources(root: &Path, files: &[LeanFile]) -> io::Result<String> {
    let mut sources = String::new();
    for file in files {
        for byte in unit::source_hash(&fs::read(root.join(&file.path))?) {
            let _ = write!(sources, "{byte:02x}");
        }
        let _ = writeln!(sources, " {}", file.path);
    }

    Ok(sources)
}

/// Whether `file` judges `translation`: `file` is a `Module.lean` or an unreached Lean file, and `translation`'s
/// namespace is the namespace of its folder or sits inside it.
pub(crate) fn judges(file: &LeanFile, translation: &Translation) -> bool {
    file.role != Role::Imported && under(&translation.facts.namespace, &file.namespace)
}

/// Builds every one of `files` in `package`, refuses every file under the namespace of a `Module.lean` or an unreached
/// Lean file Lean rejects, and runs every rule of the other `Module.lean` files over the facts of every file of
/// `translations`. A rule the runner does not run refuses every file its `Module.lean` judges. `root` is the
/// workspace's, and `roots` its PSR-4 folders.
pub(crate) fn check(
    root: &Path,
    roots: &[Root],
    library: &Path,
    package: &Package,
    files: &[LeanFile],
    translations: &[Translation],
    issues: &mut HashMap<FileId, IssueCollection>,
) -> io::Result<()> {
    let failed = package.build(&files.iter().map(|file| file.module.clone()).collect::<Vec<_>>())?;
    // The law step reports Lean's errors in the `Module.lean` beside a `Module.sharp` on that `Module.sharp`.
    let proved: HashSet<FileId> = issues.keys().copied().collect();
    let mut built = Vec::new();
    for file in files {
        if !failed.contains(&file.module) {
            if file.role == Role::Rules {
                built.push(file);
            }
            continue;
        }

        let judged: Vec<&Translation> = translations
            .iter()
            .filter(|translation| {
                judges(file, translation)
                    && !(proof_file(&translation.name) == file.path.as_bytes() && proved.contains(&translation.file))
            })
            .collect();
        if judged.is_empty() {
            continue;
        }
        let (cause, errors) = errors(root, roots, package, files, &failed, file)?;
        for translation in judged {
            let anchor = translation.facts.anchor;
            for error in &errors {
                let issue = match file.role {
                    Role::Rules if cause.path == file.path => {
                        issues::rejected_rules(anchor, &file.path, &file.namespace, error)
                    }
                    Role::Rules => {
                        issues::rejected_rules_import(anchor, &file.path, &file.namespace, &cause.path, error)
                    }
                    _ => issues::rejected_lean(anchor, &cause.path, &file.namespace, error),
                };
                issues.entry(translation.file).or_default().push(issue);
            }
        }
    }
    if built.is_empty() {
        return Ok(());
    }

    let modules: Vec<(&str, &str)> =
        built.iter().map(|rules| (rules.module.as_str(), rules.namespace.as_str())).collect();
    let names: Vec<String> =
        translations.iter().map(|translation| String::from_utf8_lossy(&translation.name).into_owned()).collect();
    let facts: Vec<(&str, &Facts)> =
        names.iter().zip(translations).map(|(name, translation)| (name.as_str(), &translation.facts)).collect();
    for broken in runner::rules(library, package, &modules, &facts)? {
        let rules = built[broken.module];
        if let Some(refused) = &broken.refused {
            for translation in translations.iter().filter(|translation| judges(rules, translation)) {
                let issue = issues::refused_rule(
                    translation.facts.anchor,
                    &rules.path,
                    &rules.namespace,
                    &broken.rule,
                    refused,
                );
                issues.entry(translation.file).or_default().push(issue);
            }
        }
        for (file, fact) in broken.facts {
            let translation = &translations[file];
            let fact = &translation.facts.list[fact];
            let issue = issues::broken_rule(fact.span, &fact.describe(), &broken.rule, &rules.path);
            issues.entry(translation.file).or_default().push(issue);
        }
    }

    Ok(())
}

/// The errors that made `file` fail, and the file that holds them: the import of a Lean file in a PSR-4 folder under a
/// root Lean or Mago owns, which cannot be built, or of a workspace Lean file that does not exist, whose build output an
/// earlier build may have left for Lean to read, else the errors of the first of `files` it imports that failed,
/// followed down, else Lean's errors in `file`.
fn errors<'file>(
    root: &Path,
    roots: &[Root],
    package: &Package,
    files: &'file [LeanFile],
    failed: &[String],
    file: &'file LeanFile,
) -> io::Result<(&'file LeanFile, Vec<Message>)> {
    let mut cause = file;
    // Lean refuses an import cycle, so a cycle among failed files is followed at most once around.
    for _ in 0..files.len() {
        let text = fs::read_to_string(root.join(&cause.path))?;
        for (line, import) in imports(&text) {
            for owned in package::owned_roots(roots) {
                if let Some(path) = file_in(root, owned, import)
                    && root.join(&path).is_file()
                {
                    return Ok((cause, vec![Message::error(line, issues::owned_root(&owned.namespace, &path))]));
                }
            }
            if let Some(path) = workspace_file(root, roots, import)
                && !root.join(String::from_utf8_lossy(&path).as_ref()).is_file()
            {
                let message = format!("bad import {import}: {} does not exist", String::from_utf8_lossy(&path));

                return Ok((cause, vec![Message::error(line, message)]));
            }
        }
        let Some(imported) = imports(&text)
            .find_map(|(_, import)| files.iter().find(|file| file.module == import && failed.contains(&file.module)))
        else {
            break;
        };
        cause = imported;
    }
    let mut errors = package.errors(&package::source_of(&cause.module))?;
    if errors.is_empty() {
        errors.push(Message::error(0, "Lake failed".to_owned()));
    }

    Ok((cause, errors))
}

fn lean_file(path: String, module: String, role: Role) -> LeanFile {
    let namespace = module.rsplit_once('.').map_or("", |(namespace, _)| namespace).to_owned();

    LeanFile { path, module, namespace, role }
}

fn is_module_lean(path: &str) -> bool {
    path.rsplit('/').next() == Some("Module.lean")
}

/// The workspace-relative path of the Lean module `module`, in the PSR-4 folder Lake links its namespace to.
fn workspace_file(root: &Path, roots: &[Root], module: &str) -> Option<Vec<u8>> {
    package::linked_roots(roots).into_iter().find_map(|linked| Some(file_in(root, linked, module)?.into_bytes()))
}

/// The workspace-relative path the Lean module `module` would have in the PSR-4 folder `folder`.
fn file_in(root: &Path, folder: &Root, module: &str) -> Option<String> {
    let rest = module.strip_prefix(folder.namespace.as_str())?.strip_prefix('.')?;
    let file = rest.split('.').fold(folder.directory.clone(), |path, name| path.join(name)).with_extension("lean");

    Some(slashed(file.strip_prefix(root).ok()?))
}

/// Each `.lean` file under `folder`, apart from hidden entries, linked folders and the `vendor` folder.
fn walk(folder: &Path, vendor: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if entry.file_name().to_string_lossy().starts_with('.') || path == vendor {
            continue;
        }
        if kind.is_dir() {
            walk(&path, vendor, found)?;
        } else if path.extension().is_some_and(|extension| extension == "lean") {
            found.push(path);
        }
    }

    Ok(())
}

/// `path` with `/` separators.
fn slashed(path: &Path) -> String {
    path.components().map(|component| component.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// The modules the header of the Lean file `text` imports, each with its 1-based line.
fn imports(text: &str) -> impl Iterator<Item = (u32, &str)> {
    text.lines().zip(1..).filter_map(|(line, number)| Some((number, line.trim().strip_prefix("import ")?.trim())))
}

/// `name` is the namespace `namespace` or sits inside it, as `Sharp.under` reads it.
fn under(name: &str, namespace: &str) -> bool {
    name.strip_prefix(namespace).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}
