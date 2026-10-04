use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use foldhash::HashMap;
use mago_database::DatabaseReader;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use serde::Deserialize;
use serde::Serialize;
use walkdir::WalkDir;

use crate::Runner;
use crate::Structure;
use crate::Violation;
use crate::error::StructureError;
use crate::runner::LIBRARY;
use crate::runner::TOOLCHAIN;
use crate::runner::build_package;

const OUTPUT: &str = ".mago/structure";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Lakefile<'rules> {
    name: &'rules str,
    default_targets: [&'rules str; 1],
    require: [Require<'rules>; 1],
    #[serde(rename = "lean_lib")]
    lean_lib: [LeanLib<'rules>; 1],
}

#[derive(Serialize)]
struct Require<'rules> {
    name: &'rules str,
    path: &'rules Path,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LeanLib<'rules> {
    name: &'rules str,
    src_dir: &'rules str,
    roots: Vec<&'rules str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    severity: String,
    data: String,
    pos: MessagePosition,
    file_name: String,
}

#[derive(Debug, Deserialize)]
struct MessagePosition {
    line: u32,
    column: u32,
}

/// The rules files of a workspace, compiled to Lean modules `sharp-rules` imports.
#[derive(Debug)]
pub struct Rules {
    package: PathBuf,
    output: PathBuf,
    modules: Vec<(String, PathBuf)>,
}

/// Checks every rules file `X.lean` beside a folder `X/` over the facts of every file.
///
/// # Errors
///
/// Returns [`StructureError`] when a rules file cannot be compiled or `sharp-rules` fails.
pub fn check(
    workspace: &Path,
    source_paths: &[String],
    structures: &[Structure],
    database: &impl DatabaseReader,
) -> Result<IssueCollection, StructureError> {
    let (rules, mut issues) = Rules::compile(workspace, source_paths)?;
    if rules.modules.is_empty() {
        return Ok(issues);
    }

    let files = structures
        .iter()
        .map(|structure| Ok((structure.file.clone(), structure.encode()?)))
        .collect::<Result<HashMap<_, _>, StructureError>>()?;
    let every: Vec<&str> = files.keys().map(String::as_str).collect();
    let mut runner = Runner::start(&rules)?;
    issues.extend(runner.check(&files, &every, &[])?.map(|violation| rules.issue(violation, database)));

    Ok(issues)
}

impl Rules {
    /// Builds every rules file `X.lean` beside a folder `X/` under the source paths with Lake, as
    /// the library `Rules` of a package Mago writes to `.mago/structure/`, so unchanged files are
    /// skipped and a rules file builds after the rules files it imports. Each file is the module
    /// named by its path under its source path: `src/Illuminate/Support.lean` is
    /// `Illuminate.Support`. Returns the rules that built, and one issue per Lean error in a file
    /// that did not; a file importing a file that did not build is left out.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError`] when `sharp-rules` cannot be built, or `lake` or `lean` cannot run.
    pub fn compile(workspace: &Path, source_paths: &[String]) -> Result<(Self, IssueCollection), StructureError> {
        let mut rules = Self { package: build_package()?, output: workspace.join(OUTPUT), modules: Vec::new() };
        let found: Vec<(String, PathBuf)> = find_rules(workspace, source_paths, &rules.output)?
            .into_iter()
            .map(|(root, file)| (module_name(&root, &file), file))
            .collect();

        let mut issues = IssueCollection::new();
        if found.is_empty() {
            return Ok((rules, issues));
        }

        rules.write_package(&found)?;
        let mut broken = rules.build()?;
        for (module, file) in &found {
            if broken.contains(module) {
                issues.extend(errors(&rules, file, workspace)?.into_iter().map(Message::into_issue));
            }
        }

        while let Some((module, _)) = found.iter().find(|(module, file)| {
            !broken.contains(module) && imports(file).iter().any(|import| broken.contains(import))
        }) {
            broken.push(module.clone());
        }

        rules.modules = found
            .into_iter()
            .filter(|(module, _)| !broken.contains(module))
            .map(|(module, file)| {
                let relative = file.strip_prefix(workspace).map(Path::to_path_buf).unwrap_or(file);
                (module, relative)
            })
            .collect();

        Ok((rules, issues))
    }

    /// Writes the package that builds the rules files: its `lakefile.toml`, its Lean version, and
    /// `src/` with a link at each module's path to its rules file. A changed `lakefile.toml` drops
    /// Lake's manifest, so Lake resolves `sharp-rules`' package again.
    fn write_package(&self, found: &[(String, PathBuf)]) -> Result<(), StructureError> {
        let lakefile = toml::to_string(&Lakefile {
            name: "rules",
            default_targets: ["Rules"],
            require: [Require { name: "sharp", path: &self.package }],
            lean_lib: [LeanLib {
                name: "Rules",
                src_dir: "src",
                roots: found.iter().map(|(module, _)| module.as_str()).collect(),
            }],
        })
        .map_err(io::Error::other)?;

        fs::create_dir_all(&self.output)?;
        if fs::read_to_string(self.output.join("lakefile.toml")).ok().as_deref() != Some(lakefile.as_str()) {
            fs::write(self.output.join("lakefile.toml"), lakefile)?;
            match fs::remove_file(self.output.join("lake-manifest.json")) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
        }

        fs::write(self.output.join("lean-toolchain"), TOOLCHAIN)?;

        let sources = self.output.join("src");
        if sources.exists() {
            fs::remove_dir_all(&sources)?;
        }

        for (module, file) in found {
            let link = sources.join(module.replace('.', "/")).with_extension("lean");
            fs::create_dir_all(link.parent().unwrap_or(&sources))?;
            symlink(file, &link)?;
        }

        Ok(())
    }

    /// Runs `lake build` and returns the modules Lake reports as failed.
    fn build(&self) -> Result<Vec<String>, StructureError> {
        let result =
            Command::new("lake").arg("build").current_dir(&self.output).output().map_err(|error| {
                match error.kind() {
                    io::ErrorKind::NotFound => StructureError::MissingLean(error),
                    _ => StructureError::IoError(error),
                }
            })?;
        if result.status.success() {
            return Ok(Vec::new());
        }

        let output = format!("{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
        let failed: Vec<String> = output
            .lines()
            .skip_while(|line| *line != "Some required targets logged failures:")
            .skip(1)
            .map_while(|line| line.strip_prefix("- "))
            .map(str::to_owned)
            .collect();
        if failed.is_empty() {
            return Err(StructureError::LeanFailed { file: self.output.join("lakefile.toml"), output });
        }

        Ok(failed)
    }

    /// The module names of the rules files that compiled.
    pub fn modules(&self) -> impl Iterator<Item = &str> {
        self.modules.iter().map(|(module, _)| module.as_str())
    }

    pub(crate) fn package(&self) -> &Path {
        &self.package
    }

    pub(crate) fn lean_path(&self) -> Result<OsString, StructureError> {
        std::env::join_paths([self.package.join(LIBRARY), self.output.join(LIBRARY)])
            .map_err(|error| StructureError::IoError(io::Error::other(error)))
    }

    /// The `broken-rule` issue for `violation`, placed on the PHP bytes of the fact that breaks it.
    pub fn issue(&self, violation: &Violation, database: &impl DatabaseReader) -> Issue {
        let issue = Issue::error(format!("Rule `{}` does not hold", violation.rule)).with_code("broken-rule");
        let rules_file = self
            .modules
            .iter()
            .find(|(module, _)| *module == violation.module)
            .map_or_else(|| violation.module.clone(), |(_, file)| file.display().to_string());

        match database.get_by_name(violation.file.as_bytes()).ok().and_then(|file| violation.location.span(&file)) {
            Some(span) => issue.with_annotation(Annotation::primary(span).with_message(&violation.description)),
            None => issue.with_note(format!(
                "{}:{}:{}: {}",
                violation.file, violation.location.start_line, violation.location.start_column, violation.description
            )),
        }
        .with_note(format!("The rule is stated in `{rules_file}`."))
    }
}

fn find_rules(
    workspace: &Path,
    source_paths: &[String],
    output: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>, StructureError> {
    let roots = if source_paths.is_empty() {
        vec![workspace.to_path_buf()]
    } else {
        source_paths.iter().map(|path| workspace.join(path)).collect()
    };

    let mut rules = Vec::new();
    for root in roots.iter().filter(|root| root.is_dir()) {
        for entry in WalkDir::new(root).sort_by_file_name().into_iter().filter_entry(|entry| entry.path() != output) {
            let path = entry.map_err(io::Error::from)?.into_path();
            if path.extension().is_some_and(|extension| extension == "lean") && path.with_extension("").is_dir() {
                rules.push((root.clone(), path));
            }
        }
    }

    rules.sort();
    rules.dedup();

    Ok(rules)
}

fn module_name(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file).with_extension("");

    relative.components().map(|component| component.as_os_str().to_string_lossy()).collect::<Vec<_>>().join(".")
}

/// The modules a rules file's header imports.
fn imports(file: &Path) -> Vec<String> {
    let header = fs::read_to_string(file).unwrap_or_default();
    let mut words = header.split_whitespace();
    let mut imports = Vec::new();
    while let Some(word) = words.next() {
        if word == "import"
            && let Some(module) = words.next()
        {
            imports.push(module.to_owned());
        }
    }

    imports
}

#[cfg(unix)]
fn symlink(file: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(file, link)
}

#[cfg(windows)]
fn symlink(file: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(file, link)
}

/// Elaborates one rules file Lake failed to build and returns its Lean errors.
fn errors(rules: &Rules, file: &Path, workspace: &Path) -> Result<Vec<Message>, StructureError> {
    let result = Command::new("lean")
        .env("LEAN_PATH", rules.lean_path()?)
        .arg("--json")
        .arg(file.strip_prefix(workspace).unwrap_or(file))
        .current_dir(workspace)
        .output()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => StructureError::MissingLean(error),
            _ => StructureError::IoError(error),
        })?;

    let output = || format!("{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    let messages: Vec<Message> = String::from_utf8_lossy(&result.stdout)
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|_| StructureError::LeanFailed { file: file.to_path_buf(), output: output() })?;

    let errors: Vec<Message> = messages.into_iter().filter(|message| message.severity == "error").collect();
    if errors.is_empty() && !result.status.success() {
        return Err(StructureError::LeanFailed { file: file.to_path_buf(), output: output() });
    }

    Ok(errors)
}

impl Message {
    fn into_issue(self) -> Issue {
        Issue::error(format!(
            "Lean rejects `{}` at line {}, column {}",
            self.file_name,
            self.pos.line,
            self.pos.column + 1
        ))
        .with_code("unproven-rule")
        .with_note(self.data)
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_database::ReadDatabase;
    use mago_database::file::File;
    use mago_span::Position;
    use mago_span::Span;

    use super::*;
    use crate::Location;

    fn file() -> File {
        File::ephemeral(Cow::Borrowed(b"src/a.php"), Cow::Borrowed("<?php\n$é = 1;\nuse A\\B;\n".as_bytes()))
    }

    fn rules() -> Rules {
        Rules {
            package: PathBuf::new(),
            output: PathBuf::new(),
            modules: vec![("App".to_owned(), PathBuf::from("src/App.lean"))],
        }
    }

    fn violation(file: &str) -> Violation {
        Violation {
            file: file.to_owned(),
            location: Location { start_line: 2, start_column: 1, end_line: 2, end_column: 3 },
            rule: "App.rule".to_owned(),
            module: "App".to_owned(),
            description: "imports A.B".to_owned(),
        }
    }

    #[test]
    fn places_a_broken_rule_on_the_bytes_of_its_fact() {
        let issue = rules().issue(&violation("src/a.php"), &ReadDatabase::single(file()));

        assert_eq!(issue.code.as_deref(), Some("broken-rule"));
        assert_eq!(issue.message, "Rule `App.rule` does not hold");
        assert_eq!(issue.primary_span(), Some(Span::new(file().id, Position::new(6), Position::new(9))));
        assert_eq!(
            issue.primary_annotation().and_then(|annotation| annotation.message.as_deref()),
            Some("imports A.B")
        );
        assert_eq!(issue.notes, ["The rule is stated in `src/App.lean`."]);
    }

    #[test]
    fn names_the_location_of_a_fact_outside_the_database() {
        let issue = rules().issue(&violation("src/missing.php"), &ReadDatabase::single(file()));

        assert_eq!(issue.primary_span(), None);
        assert_eq!(issue.notes, ["src/missing.php:2:1: imports A.B", "The rule is stated in `src/App.lean`."]);
    }

    #[test]
    fn names_modules_after_their_path_under_the_source_root() {
        assert_eq!(module_name(Path::new("/w/src"), Path::new("/w/src/Illuminate/Support.lean")), "Illuminate.Support");
        assert_eq!(module_name(Path::new("/w"), Path::new("/w/App.lean")), "App");
    }
}
