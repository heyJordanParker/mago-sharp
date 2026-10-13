use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_collector::DeferredPragmas;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_reporting::AnnotationKind;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_syntax::dialect::Dialect;

use crate::error::OrchestratorError;

/// Returns the analyzer issues of one file to report beside its semantic issues. In a PHP# file, an analyzer issue
/// on a line that holds a semantic error, or whose span holds one, as `exit(` does with a refused argument on the
/// next line, is dropped as a follow-on of that refusal. The file cannot run until the refusal is fixed, so an issue
/// dropped there shows up one fix later at worst. A PHP file keeps every analyzer issue.
pub(super) fn drop_follow_on_issues(
    file: &File,
    semantic_issues: &IssueCollection,
    analyzer_issues: IssueCollection,
) -> IssueCollection {
    if !Dialect::of(file).is_sharp() {
        return analyzer_issues;
    }

    let refusals: Vec<_> = semantic_issues
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .filter_map(|issue| issue.primary_span())
        .filter(|span| span.file_id == file.id)
        .collect();
    let refused_lines: HashSet<u32> = refusals.iter().map(|span| file.line_number(span.start.offset)).collect();

    analyzer_issues
        .into_iter()
        .filter(|issue| {
            issue.primary_span().is_none_or(|span| {
                span.file_id != file.id
                    || !(refused_lines.contains(&file.line_number(span.start.offset))
                        || refusals.iter().any(|refusal| !refusal.is_before(&span) && !span.is_before(refusal)))
            })
        })
        .collect()
}

pub(super) struct DeferredIssueReconciler {
    states: HashMap<FileId, DeferredPragmas>,
    files: HashMap<FileId, Arc<File>>,
}

impl DeferredIssueReconciler {
    pub(super) fn new(
        states: impl IntoIterator<Item = DeferredPragmas>,
        files: impl IntoIterator<Item = Arc<File>>,
    ) -> Self {
        let states = states.into_iter().map(|state| (state.file_id(), state)).collect::<HashMap<_, _>>();
        let files = if states.is_empty() {
            HashMap::default()
        } else {
            files.into_iter().filter(|file| states.contains_key(&file.id)).map(|file| (file.id, file)).collect()
        };

        Self { states, files }
    }

    pub(super) fn reconcile(&mut self, issues: IssueCollection) -> Result<IssueCollection, OrchestratorError> {
        if self.states.is_empty() || issues.is_empty() {
            return Ok(issues);
        }

        let mut passthrough = IssueCollection::new();
        let mut by_file: HashMap<FileId, IssueCollection> = HashMap::default();
        for issue in issues {
            let file_id = issue
                .annotations
                .iter()
                .find(|annotation| annotation.kind == AnnotationKind::Primary)
                .map(|annotation| annotation.span.file_id);
            if let Some(file_id) = file_id
                && self.states.contains_key(&file_id)
            {
                by_file.entry(file_id).or_default().push(issue);
            } else {
                passthrough.push(issue);
            }
        }

        for (file_id, issues) in by_file {
            let file = self.files.get(&file_id).ok_or_else(|| {
                OrchestratorError::General(format!(
                    "Failed to reconcile deferred analyzer pragmas: file {file_id:?} is unavailable.",
                ))
            })?;
            if let Some(state) = self.states.get_mut(&file_id) {
                passthrough.extend(state.reconcile(file, issues));
            }
        }

        Ok(passthrough)
    }

    pub(super) fn state(&self, file_id: FileId) -> Option<&DeferredPragmas> {
        self.states.get(&file_id)
    }

    pub(super) fn finish(self) -> Result<IssueCollection, OrchestratorError> {
        let mut issues = IssueCollection::new();
        for (file_id, state) in self.states {
            let file = self.files.get(&file_id).ok_or_else(|| {
                OrchestratorError::General(format!(
                    "Failed to finalize deferred analyzer pragmas: file {file_id:?} is unavailable.",
                ))
            })?;
            issues.extend(state.finish(file));
        }

        Ok(issues)
    }
}
