//! Structure rules: each `@[rule]` of a `Module.lean` constrains the namespace of its folder, and judges the facts of
//! every `.sharp` file under that namespace.

use std::fs;
use std::io;
use std::path::Path;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_database::file::FileId;
use mago_reporting::IssueCollection;

use crate::facts::Facts;
use crate::issues;
use crate::lean::proof_file;
use crate::package;
use crate::package::Package;
use crate::package::Root;
use crate::runner;
use crate::translate::Translation;

/// A `Module.lean` that holds the structure rules of a namespace.
#[derive(Debug)]
pub(crate) struct Rules {
    /// Workspace-relative, with `/` separators: `app/Shared/Module.lean`.
    pub(crate) path: String,
    /// Its Lean module, as the PSR-4 folder that holds it names it: `App.Shared.Module`.
    pub(crate) module: String,
    /// The namespace its rules constrain: `App.Shared`.
    pub(crate) namespace: String,
}

/// The `Module.lean` of each folder that holds the workspace-relative file `name`, up to the workspace root: the files
/// whose structure rules may judge it.
#[must_use]
pub fn rules_files(name: &[u8]) -> Vec<Vec<u8>> {
    std::iter::once(&name[..0])
        .chain(name.iter().enumerate().filter(|(_, byte)| **byte == b'/').map(|(end, _)| &name[..=end]))
        .map(|folder| [folder, b"Module.lean"].concat())
        .collect()
}

/// Each of the `rules_files` of `translations` that a PSR-4 folder of `roots` names a module, in path order.
pub(crate) fn find(root: &Path, roots: &[Root], translations: &[Translation]) -> Vec<Rules> {
    let mut found: Vec<Rules> = Vec::new();
    for path in translations.iter().flat_map(|translation| rules_files(&translation.name)) {
        let path = String::from_utf8_lossy(&path).into_owned();
        if found.iter().any(|rules| rules.path == path) || !root.join(&path).is_file() {
            continue;
        }
        if let Some(module) = package::proof_module(roots, &root.join(&path)) {
            let namespace = module.strip_suffix(".Module").unwrap_or(&module).to_owned();
            found.push(Rules { path, module, namespace });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));

    found
}

/// Whether the rules of `rules` judge `translation`: its namespace is theirs or sits inside it.
pub(crate) fn judges(rules: &Rules, translation: &Translation) -> bool {
    under(&translation.facts.namespace, &rules.namespace)
}

/// Builds every module of `rules` in `package`, refuses every file under the namespace of one Lean rejects, and runs
/// every rule of the others over the facts of every file of `translations`. `root` is the workspace's.
pub(crate) fn check(
    root: &Path,
    library: &Path,
    package: &Package,
    rules: &[Rules],
    translations: &[Translation],
    issues: &mut HashMap<FileId, IssueCollection>,
) -> io::Result<()> {
    let failed = package.build(&rules.iter().map(|rules| rules.module.clone()).collect::<Vec<_>>())?;
    // The law step reports Lean's errors in the `Module.lean` beside a `Module.sharp` on that `Module.sharp`.
    let proved: HashSet<FileId> = issues.keys().copied().collect();
    let mut built = Vec::new();
    for file in rules {
        if !failed.contains(&file.module) {
            built.push(file);
            continue;
        }

        let text = fs::read_to_string(root.join(&file.path))?;
        let imported = imports(&text)
            .find_map(|import| rules.iter().find(|rules| rules.module == import && failed.contains(&rules.module)));
        let errors = if imported.is_some() { Vec::new() } else { package.errors(&package::source_of(&file.module))? };
        let (line, message) = errors.first().map_or((0, "Lake failed"), |error| (error.pos.line, error.data.as_str()));
        let judged = translations.iter().filter(|translation| {
            judges(file, translation)
                && !(proof_file(&translation.name) == file.path.as_bytes() && proved.contains(&translation.file))
        });
        for translation in judged {
            let anchor = translation.facts.anchor;
            let issue = match imported {
                Some(imported) => issues::rejected_rules_import(anchor, &file.path, &file.namespace, &imported.path),
                None => issues::rejected_rules(anchor, &file.path, &file.namespace, line, message),
            };
            issues.entry(translation.file).or_default().push(issue);
        }
    }
    if built.is_empty() {
        return Ok(());
    }

    let modules: Vec<(&str, &str)> =
        built.iter().map(|rules| (rules.module.as_str(), rules.namespace.as_str())).collect();
    let names: Vec<String> =
        translations.iter().map(|translation| String::from_utf8_lossy(&translation.name).into_owned()).collect();
    let files: Vec<(&str, &Facts)> =
        names.iter().zip(translations).map(|(name, translation)| (name.as_str(), &translation.facts)).collect();
    for broken in runner::rules(library, &package.directory, &modules, &files)? {
        for (file, fact) in broken.facts {
            let translation = &translations[file];
            let fact = &translation.facts.list[fact];
            let issue = issues::broken_rule(fact.span, &fact.describe(), &broken.rule, &built[broken.module].path);
            issues.entry(translation.file).or_default().push(issue);
        }
    }

    Ok(())
}

/// The modules the header of the Lean file `text` imports.
fn imports(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|line| line.trim().strip_prefix("import ")).map(str::trim)
}

/// `name` is the namespace `namespace` or sits inside it, as `Sharp.under` reads it.
fn under(name: &str, namespace: &str) -> bool {
    name.strip_prefix(namespace).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}
