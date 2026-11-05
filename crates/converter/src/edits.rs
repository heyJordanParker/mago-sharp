//! The edits a conversion makes on the plain PHP file's own text, which keep its comments and layout.
//!
//! Each rewrite edits only the text its node owns, never its children's, so a rewrite nested in another one is an edit
//! inside it and the two never overlap. A rewrite that moves its children, as `'x' . $count` becoming `` `x${count}` ``
//! does, first renders each child with the edits inside it, and replaces its whole span with the text it builds from
//! them, so the children's edits leave the file's list.

use mago_span::Span;
use mago_text_edit::ApplyResult;
use mago_text_edit::TextEdit;
use mago_text_edit::TextEditor;

#[derive(Debug, Default)]
pub struct Edits {
    edits: Vec<TextEdit>,
    /// Whether two edits a render took overlapped, which [`Edits::finish`] reports.
    overlapped: bool,
}

impl Edits {
    pub fn replace(&mut self, span: Span, text: impl Into<Vec<u8>>) {
        self.edits.push(TextEdit::replace(span.start.offset..span.end.offset, text));
    }

    pub fn replace_range(&mut self, start: u32, end: u32, text: impl Into<Vec<u8>>) {
        self.edits.push(TextEdit::replace(start..end, text));
    }

    pub fn insert(&mut self, offset: u32, text: impl Into<Vec<u8>>) {
        self.edits.push(TextEdit::insert(offset, text));
    }

    pub fn delete(&mut self, start: u32, end: u32) {
        self.edits.push(TextEdit::delete(start..end));
    }

    /// The text of `source` between `start` and `end` with every edit inside it applied, which leave the list. An
    /// insert at `end` belongs to what follows, so it stays.
    pub fn render(&mut self, source: &[u8], start: u32, end: u32) -> String {
        let (inside, outside): (Vec<TextEdit>, Vec<TextEdit>) =
            std::mem::take(&mut self.edits).into_iter().partition(|edit| {
                edit.range.start >= start
                    && edit.range.end <= end
                    && !(edit.range.start == end && edit.range.is_empty())
            });
        self.edits = outside;

        let mut editor = TextEditor::new(&source[start as usize..end as usize]);
        let shifted = inside
            .into_iter()
            .map(|edit| TextEdit::replace(edit.range.start - start..edit.range.end - start, edit.new_text))
            .collect();
        if !matches!(editor.apply_batch(shifted, None::<fn(&[u8]) -> bool>), ApplyResult::Applied) {
            self.overlapped = true;
        }

        String::from_utf8_lossy(&editor.finish()).into_owned()
    }

    /// `source` with every edit applied, or `None` when two edits overlap, which is a defect of a rewrite.
    pub fn finish(self, source: &[u8]) -> Option<Vec<u8>> {
        if self.overlapped {
            return None;
        }
        let mut editor = TextEditor::new(source);
        let mut edits = self.edits;
        // Inserts at one offset keep the order they were made in.
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        for edit in edits {
            if !matches!(editor.apply(edit, None::<fn(&[u8]) -> bool>), ApplyResult::Applied) {
                return None;
            }
        }

        Some(editor.finish())
    }
}
