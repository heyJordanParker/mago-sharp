#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_span::HasSpan;
use mago_syntax::cst::Node;
use mago_syntax::cst::NodeKind;
use mago_syntax::parser::parse_file;
use mago_syntax::walker::Walker;

/// A node by its kind and the offsets it spans.
type Entered = (NodeKind, u32, u32);

fn entered(node: &Node<'_, '_>) -> Entered {
    let span = node.span();

    (node.kind(), span.start.offset, span.end.offset)
}

struct Entering;

impl<'ast, 'arena> Walker<'ast, 'arena, HashSet<Entered>> for Entering {
    fn walk_in_node(&self, node: Node<'ast, 'arena>, nodes: &mut HashSet<Entered>) {
        nodes.insert(entered(&node));
    }
}

/// Every formatter case's input, and the PHP# slice fixtures.
fn sources() -> Vec<(String, Vec<u8>)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sources = Vec::new();
    for case in std::fs::read_dir(crates.join("formatter/tests/cases")).expect("the formatter cases") {
        let path = case.expect("a formatter case").path().join("before.php");
        if let Ok(contents) = std::fs::read(&path) {
            sources.push((path.display().to_string(), contents));
        }
    }

    for fixture in ["slice.sharp", "library.sharp"] {
        let fixture = crates.join("semantics/tests/fixtures").join(fixture);
        sources.push((fixture.display().to_string(), std::fs::read(&fixture).expect("a slice fixture")));
    }

    sources
}

/// The path and contents of the PHP# slice fixture, which holds every form of the slice.
fn slice_fixture() -> (String, Vec<u8>) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../semantics/tests/fixtures/slice.sharp");

    (fixture.display().to_string(), std::fs::read(&fixture).expect("the slice fixture"))
}

/// The checks that run in one walk, such as PHP#'s slice check, see every node `Node::visit_children` reaches. Keywords
/// are left out, because the walker skips some of them, such as a closure's `use`.
#[test]
fn the_walker_enters_every_node_visit_children_reaches() {
    let mut parsed = 0;
    for (name, contents) in sources() {
        let arena = LocalArena::new();
        let file = File::ephemeral(Cow::Owned(name.clone().into_bytes()), Cow::Owned(contents));
        let program = parse_file(&arena, &file);
        if !program.errors.is_empty() {
            continue;
        }

        parsed += 1;
        let mut walked = HashSet::new();
        Entering.walk_program(program, &mut walked);
        let reached =
            Node::Program(program).filter_map(|node| (node.kind() != NodeKind::Keyword).then(|| entered(node)));
        let missed: Vec<&Entered> = reached.iter().filter(|node| !walked.contains(node)).collect();

        assert!(missed.is_empty(), "the walker misses these nodes of {name}: {missed:?}");
    }

    assert_eq!(parsed, 469, "the number of sources without a parse error");
}

/// The walker enters every type parameter list, type parameter, bound and type argument list of the slice fixture: the
/// lists of `Page`, `convert`, `Listing`, `Source` and `Comparable`, the bounds of `Page`, `convert` and `Listing`, and
/// the type arguments of its types, headers, `new`, method calls and null-safe method calls.
#[test]
fn the_walker_enters_every_type_parameter_and_type_argument_list() {
    let (name, contents) = slice_fixture();
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Owned(name.into_bytes()), Cow::Owned(contents));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "{:#?}", program.errors);

    let mut walked = HashSet::new();
    Entering.walk_program(program, &mut walked);
    let count = |kind: NodeKind| walked.iter().filter(|(entered, _, _)| *entered == kind).count();

    assert_eq!(
        [
            count(NodeKind::TypeParameterList),
            count(NodeKind::TypeParameter),
            count(NodeKind::TypeParameterBound),
            count(NodeKind::TypeArgumentList),
        ],
        [5, 6, 4, 28]
    );
}
