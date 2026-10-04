use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_database::DatabaseReader;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::Location;
use crate::Structure;
use crate::error::StructureError;

const FACTS: &str = include_str!("../lean/Sharp/Facts.lean");
const OUTPUT: &str = ".mago/structure";

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

impl Structure {
    /// # Errors
    ///
    /// Returns [`StructureError`] when the package cannot be written, `lean` cannot run, or Lean
    /// fails without reporting an error message.
    pub fn check(
        &self,
        workspace: &Path,
        source_paths: &[String],
        database: &impl DatabaseReader,
    ) -> Result<IssueCollection, StructureError> {
        let output = workspace.join(OUTPUT);
        fs::create_dir_all(output.join("Sharp"))?;
        fs::write(output.join("Sharp/Facts.lean"), FACTS)?;
        fs::write(output.join("Sharp.lean"), self.to_string())?;
        compile(&output, "Sharp/Facts")?;
        compile(&output, "Sharp")?;

        let mut issues = IssueCollection::new();
        for rules in find_rules(workspace, source_paths, &output)? {
            issues.extend(prove(workspace, &output, &rules)?.into_iter().map(|message| message.into_issue(database)));
        }

        Ok(issues)
    }
}

fn compile(output: &Path, module: &str) -> Result<(), StructureError> {
    let source = output.join(format!("{module}.lean"));
    let result =
        run(lean(output).arg("-R").arg(output).arg("-o").arg(output.join(format!("{module}.olean"))).arg(&source))?;

    if result.status.success() { Ok(()) } else { Err(failed(source, &result)) }
}

fn find_rules(workspace: &Path, source_paths: &[String], output: &Path) -> Result<Vec<PathBuf>, StructureError> {
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
                rules.push(path.strip_prefix(workspace).map(Path::to_path_buf).unwrap_or(path));
            }
        }
    }

    rules.sort();
    rules.dedup();

    Ok(rules)
}

fn prove(workspace: &Path, output: &Path, rules: &Path) -> Result<Vec<Message>, StructureError> {
    let result = run(lean(output).arg("--json").arg(rules).current_dir(workspace))?;
    let messages: Result<Vec<Message>, _> =
        String::from_utf8_lossy(&result.stdout).lines().map(serde_json::from_str).collect();
    let Ok(messages) = messages else {
        return Err(failed(rules.to_path_buf(), &result));
    };

    let errors: Vec<Message> = messages.into_iter().filter(|message| message.severity == "error").collect();
    if errors.is_empty() && !result.status.success() {
        return Err(failed(rules.to_path_buf(), &result));
    }

    Ok(errors)
}

fn lean(output: &Path) -> Command {
    let mut command = Command::new("lean");
    command.env("LEAN_PATH", output);
    command
}

fn run(command: &mut Command) -> Result<Output, StructureError> {
    command.output().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => StructureError::MissingLean(error),
        _ => StructureError::IoError(error),
    })
}

fn failed(file: PathBuf, result: &Output) -> StructureError {
    let output = format!("{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));

    StructureError::LeanFailed { file, output }
}

impl Message {
    fn into_issue(self, database: &impl DatabaseReader) -> Issue {
        if let Some((location, description, theorem)) = parse_breach(&self.data)
            && let Ok(file) = database.get_by_name(location.file.as_bytes())
            && let Some(span) = location.span(&file)
        {
            return Issue::error(format!("Rule `{theorem}` does not hold"))
                .with_code("broken-rule")
                .with_annotation(Annotation::primary(span).with_message(description))
                .with_note(format!("The rule is stated in `{}`.", self.file_name));
        }

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

fn parse_breach(data: &str) -> Option<(Location, &str, &str)> {
    let (head, theorem) = data.rsplit_once(" breaks ")?;

    head.match_indices(':').find_map(|(index, _)| {
        let (range, description) = head.get(index + 1..)?.split_once(": ")?;
        let (start, end) = range.split_once('-')?;
        let (start_line, start_column) = start.split_once('.')?;
        let (end_line, end_column) = end.split_once('.')?;
        let location = Location {
            file: head.get(..index)?.to_owned(),
            start_line: start_line.parse().ok()?,
            start_column: start_column.parse().ok()?,
            end_line: end_line.parse().ok()?,
            end_column: end_column.parse().ok()?,
        };

        Some((location, description, theorem))
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_database::ReadDatabase;
    use mago_database::file::File;
    use mago_span::Position;
    use mago_span::Span;

    use super::*;

    fn file() -> File {
        File::ephemeral(Cow::Borrowed(b"src/a.php"), Cow::Borrowed("<?php\n$é = 1;\nuse A\\B;\n".as_bytes()))
    }

    fn message(data: String) -> Message {
        Message {
            severity: "error".to_owned(),
            data,
            pos: MessagePosition { line: 3, column: 130 },
            file_name: "src/App.lean".to_owned(),
        }
    }

    #[test]
    fn places_a_broken_rule_on_the_bytes_of_its_fact() {
        let location = Location::new(&file(), 6, 9);
        let data = format!(
            "{}:{}.{}-{}.{}: imports A.B breaks App.rule",
            location.file, location.start_line, location.start_column, location.end_line, location.end_column
        );

        let issue = message(data).into_issue(&ReadDatabase::single(file()));

        assert_eq!(issue.code.as_deref(), Some("broken-rule"));
        assert_eq!(issue.message, "Rule `App.rule` does not hold");
        assert_eq!(issue.primary_span(), Some(Span::new(file().id, Position::new(6), Position::new(9))));
        assert_eq!(
            issue.primary_annotation().and_then(|annotation| annotation.message.as_deref()),
            Some("imports A.B")
        );
    }

    #[test]
    fn names_the_rules_file_for_every_other_lean_error() {
        let database = ReadDatabase::single(file());

        for data in [
            "Unknown identifier `Sharp.importz`".to_owned(),
            "src/missing.php:1.1-1.1: imports A.B breaks App.rule".to_owned(),
            "src/a.php:9.1-9.1: imports A.B breaks App.rule".to_owned(),
        ] {
            let issue = message(data.clone()).into_issue(&database);

            assert_eq!(issue.code.as_deref(), Some("unproven-rule"));
            assert_eq!(issue.message, "Lean rejects `src/App.lean` at line 3, column 131");
            assert_eq!(issue.notes, [data]);
        }
    }
}
