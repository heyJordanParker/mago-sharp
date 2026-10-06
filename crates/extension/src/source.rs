//! Compact syntax snapshots shared by external linter and analyzer protocols.

use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_span::HasSpan;
use mago_span::Position;
use mago_syntax::cst::Node;
use mago_syntax::cst::NodeKind;
use mago_syntax::cst::Program;
use mago_syntax::cst::TriviaKind;
use strum::IntoEnumIterator;

use crate::PayloadError;
use crate::PayloadWriter;

/// Sentinel used by packed node records for absent parent and sibling links.
pub const NO_NODE: u32 = u32::MAX;

/// Width of one packed syntax-node record.
pub const NODE_RECORD_SIZE: usize = 21;

/// Width of one packed resolved-name record, excluding its start column.
pub const NAME_RECORD_SIZE: usize = 14;

/// Width of one packed comment-trivia record.
pub const TRIVIA_RECORD_SIZE: usize = 9;

/// Writes the ordered node-kind table used to validate SDK compatibility.
pub fn write_node_kind_table(writer: &mut PayloadWriter) {
    writer.write_u32(NodeKind::iter().count() as u32);
    for kind in NodeKind::iter() {
        let name = kind.to_string();
        writer.write_u32(name.len() as u32);
        writer.write_raw(name.as_bytes());
    }
}

#[derive(Debug)]
struct SnapshotNode {
    kind: NodeKind,
    start: u32,
    end: u32,
    parent: Option<u32>,
    first_child: Option<u32>,
    next_sibling: Option<u32>,
    last_child: Option<u32>,
}

/// A flat syntax tree, resolved-name table, and comment table ready for a
/// capability-specific extension protocol to encode.
#[derive(Debug)]
pub struct SourceSnapshot<'arena> {
    nodes: Vec<SnapshotNode>,
    targets: Vec<u32>,
    /// Each resolved name as `(start, end, name, imported, binding)`, the binding encoded by [`binding_code`].
    names: Vec<(u32, u32, &'arena [u8], bool, u8)>,
    trivia: Vec<(u8, u32, u32)>,
}

impl<'arena> SourceSnapshot<'arena> {
    /// Builds a complete snapshot and records nodes matching analyzer-hook targets.
    ///
    /// # Errors
    ///
    /// Returns an error when the syntax tree exceeds the protocol's `u32`
    /// address space.
    pub fn complete_with_targets<'ast>(
        program: &'ast Program<'arena>,
        resolved_names: &ResolvedNames<'arena>,
        target_kinds: Option<&[bool; u8::MAX as usize + 1]>,
    ) -> Result<Self, PayloadError> {
        Self::complete_with_target_filter(program, resolved_names, |node| {
            target_kinds.is_some_and(|target_kinds| target_kinds[node.kind() as usize])
        })
    }

    /// Builds a complete snapshot and records nodes accepted by `is_target`.
    ///
    /// # Errors
    ///
    /// Returns an error when the syntax tree exceeds the protocol's `u32`
    /// address space.
    pub fn complete_with_target_filter<'ast>(
        program: &'ast Program<'arena>,
        resolved_names: &ResolvedNames<'arena>,
        mut is_target: impl FnMut(Node<'ast, 'arena>) -> bool,
    ) -> Result<Self, PayloadError> {
        let mut nodes = Vec::new();
        let mut targets = Vec::new();
        let mut stack = Vec::with_capacity(64);
        Self::append_subtree(Node::Program(program), &mut is_target, &mut nodes, &mut targets, &mut stack)?;

        let mut names = resolved_names.iter().map(|name| with_binding(resolved_names, name)).collect::<Vec<_>>();
        names.sort_unstable_by_key(|(start, end, _, _, _)| (*start, *end));

        Ok(Self { nodes, targets, names, trivia: collect_trivia(program) })
    }

    /// Builds only syntax subtrees needed by active external linter rules.
    ///
    /// Returns `None` when the file has no matching target nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when the filtered syntax tree exceeds the protocol's
    /// `u32` address space.
    pub fn filtered<'ast>(
        program: &'ast Program<'arena>,
        resolved_names: &ResolvedNames<'arena>,
        target_kinds: &[bool; u8::MAX as usize + 1],
    ) -> Result<Option<Self>, PayloadError> {
        Self::targeted_with_filter(
            program,
            resolved_names,
            |node| target_kinds[node.kind() as usize].then_some(true),
            true,
        )
    }

    /// Builds the minimal syntax snapshot for dynamically selected targets.
    ///
    /// The selector returns `Some(include_subtree)` for target nodes and `None`
    /// for unrelated nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when the filtered syntax tree exceeds the protocol's
    /// `u32` address space.
    pub fn targeted_with_filter<'ast>(
        program: &'ast Program<'arena>,
        resolved_names: &ResolvedNames<'arena>,
        mut target: impl FnMut(Node<'ast, 'arena>) -> Option<bool>,
        include_trivia: bool,
    ) -> Result<Option<Self>, PayloadError> {
        let mut nodes = Vec::new();
        let mut targets = Vec::new();
        let mut included_ranges = Vec::new();
        let mut stack = Vec::with_capacity(64);
        let mut subtree_stack = Vec::with_capacity(64);
        stack.push(Node::Program(program));
        while let Some(node) = stack.pop() {
            let target_configuration = target(node);
            if target_configuration == Some(true) {
                let span = node.span();
                included_ranges.push((span.start.offset, span.end.offset));
                Self::append_subtree(
                    node,
                    &mut |child| target(child).is_some(),
                    &mut nodes,
                    &mut targets,
                    &mut subtree_stack,
                )?;
                continue;
            }

            if target_configuration.is_some() {
                let identifier =
                    u32::try_from(nodes.len()).map_err(|_| PayloadError::LengthOverflow { length: nodes.len() })?;
                let span = node.span();
                included_ranges.push((span.start.offset, span.end.offset));
                nodes.push(SnapshotNode {
                    kind: node.kind(),
                    start: span.start.offset,
                    end: span.end.offset,
                    parent: None,
                    first_child: None,
                    next_sibling: None,
                    last_child: None,
                });
                targets.push(identifier);
            }

            let start = stack.len();
            node.visit_children(|child| stack.push(child));
            stack[start..].reverse();
        }

        if targets.is_empty() {
            return Ok(None);
        }

        let included_ranges = merge_ranges(included_ranges);
        let mut names = resolved_names
            .iter()
            .filter(|(start, end, _, _)| {
                let range_index = included_ranges.partition_point(|(_, range_end)| range_end <= start);
                included_ranges
                    .get(range_index)
                    .is_some_and(|(range_start, range_end)| range_start <= start && end <= range_end)
            })
            .map(|name| with_binding(resolved_names, name))
            .collect::<Vec<_>>();
        names.sort_unstable_by_key(|(start, end, _, _, _)| (*start, *end));

        Ok(Some(Self {
            nodes,
            targets,
            names,
            trivia: if include_trivia { collect_trivia(program) } else { Vec::new() },
        }))
    }

    fn append_subtree<'ast>(
        root: Node<'ast, 'arena>,
        is_target: &mut impl FnMut(Node<'ast, 'arena>) -> bool,
        nodes: &mut Vec<SnapshotNode>,
        targets: &mut Vec<u32>,
        stack: &mut Vec<(Node<'ast, 'arena>, Option<u32>)>,
    ) -> Result<(), PayloadError> {
        stack.push((root, None));
        while let Some((node, parent)) = stack.pop() {
            let identifier =
                u32::try_from(nodes.len()).map_err(|_| PayloadError::LengthOverflow { length: nodes.len() })?;
            let span = node.span();
            nodes.push(SnapshotNode {
                kind: node.kind(),
                start: span.start.offset,
                end: span.end.offset,
                parent,
                first_child: None,
                next_sibling: None,
                last_child: None,
            });

            if let Some(parent) = parent {
                let previous_sibling = nodes[parent as usize].last_child.replace(identifier);
                if let Some(previous_sibling) = previous_sibling {
                    nodes[previous_sibling as usize].next_sibling = Some(identifier);
                } else {
                    nodes[parent as usize].first_child = Some(identifier);
                }
            }

            if is_target(node) {
                targets.push(identifier);
            }

            let start = stack.len();
            node.visit_children(|child| stack.push((child, Some(identifier))));
            stack[start..].reverse();
        }

        Ok(())
    }

    /// Writes target identifiers and packed syntax, name, and trivia tables.
    ///
    /// # Errors
    ///
    /// Returns an error when a table or name buffer exceeds `u32::MAX` bytes.
    pub fn write_to(&self, writer: &mut PayloadWriter) -> Result<(), PayloadError> {
        writer.write_length(self.targets.len())?;
        for target in &self.targets {
            writer.write_u32(*target);
        }

        writer.write_length(self.nodes.len())?;
        for node in &self.nodes {
            writer.write_u8(node.kind as u8);
            writer.write_u32(node.start);
            writer.write_u32(node.end);
            writer.write_u32(node.parent.unwrap_or(NO_NODE));
            writer.write_u32(node.first_child.unwrap_or(NO_NODE));
            writer.write_u32(node.next_sibling.unwrap_or(NO_NODE));
        }

        writer.write_length(self.names.len())?;
        for (start, _, _, _, _) in &self.names {
            writer.write_u32(*start);
        }

        let mut name_offset = 0usize;
        for (_, end, name, imported, binding) in &self.names {
            writer.write_u32(*end);
            writer.write_length(name_offset)?;
            writer.write_length(name.len())?;
            writer.write_bool(*imported);
            writer.write_u8(*binding);
            name_offset =
                name_offset.checked_add(name.len()).ok_or(PayloadError::LengthOverflow { length: usize::MAX })?;
        }

        writer.write_length(name_offset)?;
        for (_, _, name, _, _) in &self.names {
            writer.write_raw(name);
        }

        writer.write_length(self.trivia.len())?;
        for (kind, start, end) in &self.trivia {
            writer.write_u8(*kind);
            writer.write_u32(*start);
            writer.write_u32(*end);
        }

        Ok(())
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        4usize
            .saturating_add(self.targets.len().saturating_mul(4))
            .saturating_add(4)
            .saturating_add(self.nodes.len().saturating_mul(NODE_RECORD_SIZE))
            .saturating_add(4)
            .saturating_add(self.names.len().saturating_mul(4 + NAME_RECORD_SIZE))
            .saturating_add(4)
            .saturating_add(self.names.iter().map(|(_, _, name, _, _)| name.len()).sum::<usize>())
            .saturating_add(4)
            .saturating_add(self.trivia.len().saturating_mul(TRIVIA_RECORD_SIZE))
    }

    #[must_use]
    pub const fn target_count(&self) -> usize {
        self.targets.len()
    }

    #[must_use]
    pub const fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub const fn name_count(&self) -> usize {
        self.names.len()
    }

    #[must_use]
    pub const fn trivia_count(&self) -> usize {
        self.trivia.len()
    }
}

/// Adds the binding of the bare PHP# name that starts the resolved name, so a hook can tell the class `Calc` in the
/// static call `Calc.make()` from a local.
fn with_binding<'arena>(
    resolved_names: &ResolvedNames<'arena>,
    (start, end, name, imported): (u32, u32, &'arena [u8], bool),
) -> (u32, u32, &'arena [u8], bool, u8) {
    (start, end, name, imported, binding_code(resolved_names.binding(&Position::new(start))))
}

/// The byte a resolved-name record carries for its binding: `0` for a PHP name, then `1` for a class, `2` for a
/// constant and `3` for a member. Locals, `this` and `field` have no resolved name, so no record carries them.
const fn binding_code(binding: Option<Binding>) -> u8 {
    match binding {
        None | Some(Binding::Local(_) | Binding::This | Binding::Field) => 0,
        Some(Binding::Class) => 1,
        Some(Binding::Constant) => 2,
        Some(Binding::Member) => 3,
    }
}

fn merge_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, previous_end)) = merged.last_mut()
            && start <= *previous_end
        {
            *previous_end = (*previous_end).max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn collect_trivia(program: &Program<'_>) -> Vec<(u8, u32, u32)> {
    program
        .trivia
        .iter()
        .filter_map(|trivia| {
            let kind = match trivia.kind {
                TriviaKind::SingleLineComment => 1,
                TriviaKind::MultiLineComment => 2,
                TriviaKind::HashComment => 3,
                TriviaKind::DocBlockComment => 4,
                TriviaKind::WhiteSpace => return None,
            };

            Some((kind, trivia.span.start.offset, trivia.span.end.offset))
        })
        .collect()
}
