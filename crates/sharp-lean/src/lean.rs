//! The Lean step of `mago compile`: proves each law, and refuses each `.sharp` file whose law is not proved.

use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use foldhash::HashMap;
use mago_database::file::FileId;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_sharp_bridge::unit;
use mago_span::Span;
use rayon::prelude::*;

use crate::issues;
use crate::library;
use crate::package;
use crate::package::Module;
use crate::package::Package;
use crate::package::Program;
use crate::package::Root;
use crate::package::Toolchain;
use crate::runner;
use crate::runner::Answer;
use crate::runner::Question;
use crate::runner::Verdict;
use crate::translate::Law;
use crate::translate::Translation;
use crate::translate::names;

/// The folder Composer installs packages into. Mago never writes a proof there.
const VENDOR: &[u8] = b"vendor/";

/// The comment above a proof Lean's steps did not find.
const NO_PROOF_FOUND: &str = "-- PHP#: no automatic proof found";

/// The Lean step of one workspace.
#[derive(Debug)]
pub struct Lean {
    root: PathBuf,
}

/// A file the step proves: it states a law or has a proof file.
struct Work<'work> {
    translation: &'work Translation,
    /// Its proof file, relative to the workspace root.
    proof: Vec<u8>,
    exists: bool,
    /// Its proof file's Lean module.
    module: String,
}

impl Lean {
    /// The Lean step of the workspace at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Lean {
        Lean { root: root.to_path_buf() }
    }

    /// Proves the laws of `translations`, which hold every accepted `.sharp` file, and returns the issues that refuse
    /// each file whose law is not proved.
    ///
    /// A file whose `.sharpc` is current is proved: the `.sharpc` lists its proof file and every source its laws reach
    /// as inputs, and the build ID it records covers the runtime library. No Lean process starts when no file states a
    /// law or has a proof file, nor when each such file's `.sharpc` is current. When no law and no proof file remain,
    /// `.sharp/.lean/` is deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be read or written, when the runtime library does not build, or when the
    /// runner fails.
    pub fn prove(&self, translations: Vec<Translation>) -> io::Result<HashMap<FileId, IssueCollection>> {
        let package = Package::new(&self.root);
        let mut issues: HashMap<FileId, IssueCollection> = HashMap::default();

        let mut candidates = Vec::new();
        for translation in &translations {
            let proof = proof_file(&translation.name);
            let exists = self.path(&proof).is_file();
            if !translation.laws.is_empty() || exists {
                candidates.push((translation, proof, exists));
            }
        }
        if candidates.is_empty() {
            match fs::remove_dir_all(&package.directory) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => return Ok(issues),
            }
        }

        let mut stale = Vec::new();
        for candidate in candidates {
            if self.is_stale(candidate.0)? {
                stale.push(candidate);
            }
        }
        if stale.is_empty() {
            return Ok(issues);
        }

        let program = Program::new(&translations);
        let translated: Vec<FileId> = translations.iter().map(|translation| translation.file).collect();
        let roots = self.roots(stale.iter().map(|(translation, ..)| *translation))?;
        let mut work = Vec::new();
        for (translation, proof, exists) in stale {
            let mut sound = true;
            for law in &translation.laws {
                let problems = program.problems(&law.statement);
                for construct in &problems.unmodeled {
                    report(&mut issues, translation, issues::unmodeled(law, construct));
                }
                for missing in &problems.missing {
                    let issue = if missing.file.is_some_and(|file| translated.contains(&file)) {
                        issues::untranslated(law, &missing.place)
                    } else {
                        issues::refused_reach(law, &missing.place)
                    };
                    report(&mut issues, translation, issue);
                }
                sound &= problems.is_empty();
            }
            if !sound {
                continue;
            }

            let Some(module) = package::proof_module(&roots, &self.path(&proof)) else {
                let file = String::from_utf8_lossy(&proof);
                for law in &translation.laws {
                    report(&mut issues, translation, issues::no_module(law, &file));
                }
                continue;
            };
            work.push(Work { translation, proof, exists, module });
        }
        if work.is_empty() {
            return Ok(issues);
        }

        let toolchain = package.toolchain()?;
        if !matches!(toolchain, Toolchain::Ready) {
            for item in &work {
                if let Some(span) = anchor(item.translation) {
                    let issue = match &toolchain {
                        Toolchain::Failed(message) => issues::elan_failed(span, message),
                        _ => issues::lake_not_found(span),
                    };
                    report(&mut issues, item.translation, issue);
                }
            }
            return Ok(issues);
        }

        let library = library::build()?;
        let modules = program.modules(&work.iter().map(|item| item.translation.file).collect::<Vec<_>>());
        package.write(&library, &roots, &modules)?;

        let mut hosts: Vec<&Module> = Vec::new();
        for item in &work {
            let name = package::module_name(item.translation);
            if let Some(module) = modules.iter().find(|module| module.name == name)
                && !hosts.iter().any(|host| host.name == module.host)
                && let Some(host) = modules.iter().find(|host| host.name == module.host)
            {
                hosts.push(host);
            }
        }
        let failed = package.build(&hosts.iter().map(|host| host.name.clone()).collect::<Vec<_>>())?;
        let mut refused_hosts: HashMap<String, Vec<package::Message>> = HashMap::default();
        for host in failed {
            let errors = package.errors(&package::source_of(&host))?;
            refused_hosts.insert(host, errors);
        }
        work.retain(|item| {
            let name = package::module_name(item.translation);
            let host = modules.iter().find(|module| module.name == name).map_or(name, |module| module.host.clone());
            let Some(errors) = refused_hosts.get(&host) else {
                return true;
            };
            let (line, message) =
                errors.first().map_or((0, "Lake failed"), |error| (error.pos.line, error.data.as_str()));
            for law in &item.translation.laws {
                report(&mut issues, item.translation, issues::generated_module_refused(law, &host, line, message));
            }

            false
        });

        let built: Vec<String> = work.iter().filter(|item| item.exists).map(|item| item.module.clone()).collect();
        let failed = package.build(&built)?;
        let mut checked = Vec::new();
        for item in work {
            if !item.exists && item.translation.name.starts_with(VENDOR) {
                let file = String::from_utf8_lossy(&item.proof);
                for law in &item.translation.laws {
                    report(&mut issues, item.translation, issues::no_proof(law, &file));
                }
            } else if failed.contains(&item.module) {
                let errors = package.errors(&package::source_of(&item.module))?;
                let text = fs::read_to_string(self.path(&item.proof))?;
                for issue in proof_errors(item.translation, &item.proof, &text, &errors) {
                    report(&mut issues, item.translation, issue);
                }
            } else {
                checked.push(item);
            }
        }
        if checked.is_empty() {
            return Ok(issues);
        }

        // One runner per file, run on Mago's thread pool, so `threads` caps how many run at once, each near 465 MB.
        let answers: Vec<io::Result<Vec<Answer>>> = checked
            .par_iter()
            .map(|item| {
                let vendor = item.translation.name.starts_with(VENDOR);
                let questions: Vec<Question<'_>> = item
                    .translation
                    .laws
                    .iter()
                    .map(|law| Question {
                        statement: &law.statement,
                        propose: !vendor,
                        unfold: program.definitions(&law.statement),
                    })
                    .collect();
                let (module, proof_module) = if item.exists {
                    (item.module.clone(), item.module.clone())
                } else {
                    (package::module_name(item.translation), String::new())
                };

                runner::check(&library, &package.directory, &module, &proof_module, &questions)
            })
            .collect();

        for (item, answers) in checked.into_iter().zip(answers) {
            let answers = answers?;
            let vendor = item.translation.name.starts_with(VENDOR);
            let file = String::from_utf8_lossy(&item.proof).into_owned();
            let mut text = if item.exists {
                fs::read_to_string(self.path(&item.proof))?
            } else {
                format!("import {}\nopen Sharp\n", package::module_name(item.translation))
            };
            let original = text.len();
            for (law, answer) in item.translation.laws.iter().zip(answers) {
                if answer.proofs.iter().any(|proof| proof.verdict == Verdict::Proved) {
                    continue;
                }
                if let Some(proof) = answer.proofs.first() {
                    let issue = match &proof.verdict {
                        Verdict::Gap => issues::gap(law, &file, proof.line),
                        Verdict::NativeDecide => issues::native_decide(law, &file),
                        Verdict::Axiom(axiom) => issues::axiom(law, &file, axiom),
                        Verdict::Proved => continue,
                    };
                    report(&mut issues, item.translation, issue);
                    continue;
                }
                if vendor {
                    report(&mut issues, item.translation, issues::no_proof(law, &file));
                    continue;
                }

                // A proof file is only ever appended to: an existing proof stays as its author wrote it.
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push('\n');
                let theorem = format!("theorem {} : {} := by", names::identifier(law.name.as_bytes()), law.statement);
                match answer.proposal {
                    Some(step) => {
                        let _ = write!(text, "{theorem}\n  {step}\n");
                    }
                    None => {
                        let _ = write!(text, "{NO_PROOF_FOUND}\n{theorem}\n");
                        let line = u32::try_from(text.lines().count() + 1).unwrap_or(u32::MAX);
                        text.push_str("  sorry\n");
                        report(&mut issues, item.translation, issues::gap(law, &file, line));
                    }
                }
            }
            if text.len() != original || !item.exists {
                fs::write(self.path(&item.proof), text)?;
            }
        }

        Ok(issues)
    }

    /// Whether `translation`'s `.sharpc` is missing, or one of its inputs changed since it was written.
    fn is_stale(&self, translation: &Translation) -> io::Result<bool> {
        let source = translation.path.clone().unwrap_or_else(|| self.path(&translation.name));
        let Some(compiled) = unit::compiled_path(&self.root, &source)? else {
            return Ok(true);
        };

        match fs::read(compiled) {
            Ok(bytes) => unit::is_stale(&self.root, &bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// The PSR-4 folders of the workspace's `composer.json` and of each package in `vendor/` one of `translations`
    /// belongs to.
    fn roots<'translation>(
        &self,
        translations: impl Iterator<Item = &'translation Translation>,
    ) -> io::Result<Vec<Root>> {
        let mut roots = package::roots(&self.root)?;
        let mut packages: Vec<PathBuf> = Vec::new();
        for translation in translations {
            let Some(rest) = translation.name.strip_prefix(VENDOR) else {
                continue;
            };
            let mut parts = rest.split(|byte| *byte == b'/');
            if let (Some(vendor), Some(name), Some(_)) = (parts.next(), parts.next(), parts.next()) {
                let package = self
                    .path(VENDOR)
                    .join(String::from_utf8_lossy(vendor).as_ref())
                    .join(String::from_utf8_lossy(name).as_ref());
                if !packages.contains(&package) {
                    packages.push(package);
                }
            }
        }
        for package in packages {
            roots.extend(package::roots(&package)?);
        }

        Ok(roots)
    }

    /// The workspace-relative `name` as a path.
    fn path(&self, name: &[u8]) -> PathBuf {
        self.root.join(String::from_utf8_lossy(name).as_ref())
    }
}

/// The proof file of the `.sharp` file `name`: the same path with `.lean` in place of `.sharp`.
#[must_use]
pub fn proof_file(name: &[u8]) -> Vec<u8> {
    let stem = name.strip_suffix(b".sharp").unwrap_or(name);

    [stem, b".lean"].concat()
}

/// Lean's errors for a proof file that did not build. A theorem's errors give one issue, from its first error: on the
/// law the theorem proves, or on the class name for the proof of a law the file no longer states, which Lean reports
/// as an unknown name at the theorem's type. An error outside every theorem gives its own issue on the class name.
fn proof_errors(translation: &Translation, proof: &[u8], text: &str, errors: &[package::Message]) -> Vec<Issue> {
    let file = String::from_utf8_lossy(proof).into_owned();
    let source = translation.name.rsplit(|byte| *byte == b'/').next().unwrap_or(&translation.name);
    let source = String::from_utf8_lossy(source).into_owned();
    let theorems = theorems(text);
    let class = translation.class.as_ref().map(|(class, _)| class.as_str());
    let Some(anchor) = anchor(translation) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    let mut reported: Vec<u32> = Vec::new();
    for error in errors {
        let Some((line, r#type)) = theorems.iter().rev().find(|(line, _)| *line <= error.pos.line) else {
            found.push(issues::rejected_file(anchor, &file, error.pos.line, &error.data));
            continue;
        };
        if reported.contains(line) {
            continue;
        }
        reported.push(*line);

        let deleted = class.is_some_and(|class| {
            r#type.strip_prefix(class).is_some_and(|member| member.starts_with('.'))
                && !translation.laws.iter().any(|law| law.statement == *r#type)
                && errors.iter().any(|error| unknown_name(&error.data) == Some(r#type.as_str()))
        });
        if deleted {
            found.push(issues::deleted_law(anchor, &file, *line, r#type, &source));
        } else if let Some(law) = translation.laws.iter().find(|law| law.statement == *r#type) {
            found.push(issues::rejected(law, &file, error.pos.line, &error.data));
        } else {
            found.push(issues::rejected_file(anchor, &file, error.pos.line, &error.data));
        }
    }

    found
}

/// Each `theorem` of a proof file, with the line it starts on and its type as written.
fn theorems(text: &str) -> Vec<(u32, String)> {
    let mut theorems = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let Some(rest) = line.strip_prefix("theorem ") else {
            continue;
        };
        let Some((_, r#type)) = rest.split_once(" : ") else {
            continue;
        };
        let r#type = r#type.split_once(":=").map_or(r#type, |(r#type, _)| r#type).trim();
        theorems.push((u32::try_from(index + 1).unwrap_or(u32::MAX), r#type.to_owned()));
    }

    theorems
}

/// The name of Lean's ``Unknown identifier `X` `` or ``Unknown constant `X` `` message.
fn unknown_name(message: &str) -> Option<&str> {
    message
        .strip_prefix("Unknown identifier `")
        .or_else(|| message.strip_prefix("Unknown constant `"))?
        .split_once('`')
        .map(|(name, _)| name)
}

/// Where a file-wide issue sits: the file's class name, else its first law.
fn anchor(translation: &Translation) -> Option<Span> {
    translation.class.as_ref().map(|(_, span)| *span).or_else(|| translation.laws.first().map(|law: &Law| law.span))
}

fn report(issues: &mut HashMap<FileId, IssueCollection>, translation: &Translation, issue: Issue) {
    issues.entry(translation.file).or_default().push(issue);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_file_sits_beside_its_source() {
        assert_eq!(proof_file(b"app/Shared/Money.sharp"), b"app/Shared/Money.lean");
    }

    #[test]
    fn each_theorem_is_found_with_its_line_and_type() {
        let text = "import Code.App.Shared.Money\nopen Sharp\n\ntheorem addKeepsCurrency : App.Shared.Money.addKeepsCurrency := by\n  simp\n";

        assert_eq!(theorems(text), [(4, "App.Shared.Money.addKeepsCurrency".to_owned())]);
    }

    #[test]
    fn the_name_of_an_unknown_name_message_is_read() {
        assert_eq!(unknown_name("Unknown constant `App.Shared.Money.oldLaw`"), Some("App.Shared.Money.oldLaw"));
        assert_eq!(unknown_name("Unknown identifier `App.Shared.Money.oldLaw`"), Some("App.Shared.Money.oldLaw"));
        assert_eq!(unknown_name("unsolved goals"), None);
    }
}
