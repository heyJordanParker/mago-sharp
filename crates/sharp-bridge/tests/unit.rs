//! Each test lowers a PHP# file and checks the compiled file the bridge encodes from it, or the lowered unit, against
//! the layout `ext/sharp` reads.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::big_endian_bytes,
    clippy::little_endian_bytes
)]

mod common;

use std::fmt::Write;
use std::fs;
use std::mem::align_of;
use std::mem::offset_of;
use std::mem::size_of;
use std::path::Path;
use std::process::Command;
use std::slice;

use mago_build_id::BUILD_ID;
use mago_sharp_bridge::SHARP_UNIT_ABI;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::lower;
use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_value;
use mago_sharp_bridge::unit::FormatError;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Read;
use mago_sharp_bridge::unit::Reads;
use mago_sharp_bridge::unit::SHARP_UNIT_MAGIC;
use mago_sharp_bridge::unit::compiled_path;
use mago_sharp_bridge::unit::encode;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::is_stale;
use mago_sharp_bridge::unit::key;
use mago_sharp_bridge::unit::sharp_input;
use mago_sharp_bridge::unit::sharp_unit_header;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_bridge::unit::stamp;
use xxhash_rust::xxh3::xxh3_128;

const SOURCE: &str = "namespace App.Tenant;\n\nclass Report\n{\n    public string title()\n    {\n        return \"weekly\";\n    }\n}\n";

/// The unit the bridge lowers from `source`, which the checker accepts.
fn lowered(source: &str) -> Unit {
    common::checked("src/Report.sharp", source, &[], lower).expect("the checker accepts the source")
}

/// The compiled file of `source`, with the test key, inputs and facts.
fn encoded(source: &str) -> Vec<u8> {
    encode(&lowered(source), source.as_bytes(), KEY, &inputs(), FACTS)
}

const KEY: [u8; 16] = *b"0123456789abcdef";

const FACTS: &[u8] = b"facts the engine skips";

fn inputs() -> Vec<Input> {
    vec![
        Input { path: b"src/Report.sharp".to_vec(), size: 98, mtime_ns: 1_700_000_000_123_456_789, hash: [7; 16] },
        Input { path: b"composer.lock".to_vec(), size: 4096, mtime_ns: -5, hash: [9; 16] },
    ]
}

/// A compiled file read back through the C layout, as `ext/sharp` reads it: copied to memory aligned for every
/// section, then cast.
struct Decoded {
    header: sharp_unit_header,
    inputs: Vec<sharp_input>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<u8>,
    facts: Vec<u8>,
    /// Where each section starts: inputs, nodes, children, texts, facts, and the end.
    offsets: [usize; 6],
}

impl Decoded {
    fn new(bytes: &[u8]) -> Self {
        let mut memory = vec![0u64; bytes.len().div_ceil(8)];
        // SAFETY: `memory` holds at least `bytes.len()` bytes, and the two do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), memory.as_mut_ptr().cast::<u8>(), bytes.len()) };

        assert!(bytes.len() >= size_of::<sharp_unit_header>(), "the file holds a whole header");
        // SAFETY: `memory` is 8-aligned, as the header is, and holds a whole header.
        let header = unsafe { *memory.as_ptr().cast::<sharp_unit_header>() };

        let inputs_at = size_of::<sharp_unit_header>();
        let nodes_at = inputs_at + size_of::<sharp_input>() * header.input_count as usize;
        let children_at = nodes_at + size_of::<sharp_node>() * header.node_count as usize;
        let texts_at = children_at + size_of::<u32>() * header.children_count as usize;
        let facts_at = texts_at + header.texts_size as usize;
        let end = facts_at + header.facts_size as usize;
        assert_eq!(end, bytes.len(), "the file is as long as its header says");

        Self {
            header,
            inputs: section(&memory, inputs_at, header.input_count as usize),
            nodes: section(&memory, nodes_at, header.node_count as usize),
            children: section(&memory, children_at, header.children_count as usize),
            texts: section(&memory, texts_at, header.texts_size as usize),
            facts: section(&memory, facts_at, header.facts_size as usize),
            offsets: [inputs_at, nodes_at, children_at, texts_at, facts_at, end],
        }
    }

    fn text(&self, text: sharp_str) -> &[u8] {
        &self.texts[text.offset as usize..(text.offset + text.len) as usize]
    }
}

/// The `count` values of `T` from `offset` in `memory`, after checking that `offset` is aligned for `T` and that the
/// values lie inside `memory`.
fn section<T>(memory: &[u64], offset: usize, count: usize) -> Vec<T>
where
    T: Copy,
{
    assert_eq!(offset % align_of::<T>(), 0, "a section starts aligned for its type");
    assert!(offset + count * size_of::<T>() <= size_of_val(memory), "a section lies inside the file");
    let start = memory.as_ptr().cast::<u8>().wrapping_add(offset).cast::<T>();

    // SAFETY: `start` is aligned for `T`, and `count` values of `T` lie inside `memory` from it.
    unsafe { slice::from_raw_parts(start, count) }.to_vec()
}

#[test]
fn every_text_of_a_lowered_unit_is_an_offset_into_its_texts() {
    let lowered = lowered(SOURCE);
    let texts = lowered.texts();

    let resolved: Vec<&[u8]> = lowered
        .nodes()
        .iter()
        .filter(|node| node.text.len != 0)
        .map(|node| &texts[node.text.offset as usize..(node.text.offset + node.text.len) as usize])
        .collect();

    assert_eq!(resolved, [&b"strict_types"[..], b"App\\Tenant", b"weekly", b"string", b"title", b"Report"]);
    assert!(lowered.nodes().iter().any(|node| node.kind == sharp_kind::SHARP_AST_ZVAL
        && node.value == sharp_value::SHARP_STRING
        && node.text.len == 6));
}

#[test]
fn an_absent_input_reads_back_as_an_all_zero_stamp_that_no_existing_file_has() {
    let absent = Input { path: b"composer.lock".to_vec(), size: 0, mtime_ns: 0, hash: [0; 16] };

    let decoded = Decoded::new(&encode(&lowered(SOURCE), SOURCE.as_bytes(), KEY, &[absent], FACTS));

    let [input] = decoded.inputs[..] else { panic!("one input: {:?}", decoded.inputs.len()) };
    assert_eq!(decoded.text(input.path), b"composer.lock");
    assert_eq!((input.size, input.mtime_ns, input.hash), (0, 0, [0; 16]));
    assert_ne!(source_hash(b""), [0; 16], "an empty file that exists is never stamped as absent");
}

#[test]
fn a_compiled_file_reads_back_through_the_c_layout() {
    let lowered = lowered(SOURCE);
    let decoded = Decoded::new(&encode(&lowered, SOURCE.as_bytes(), KEY, &inputs(), FACTS));
    let header = decoded.header;

    assert_eq!(header.magic, *b"SHARPC\0\0");
    assert_eq!(header.magic, SHARP_UNIT_MAGIC);
    assert_eq!(header.abi, SHARP_UNIT_ABI);
    assert_eq!(header.checker, BUILD_ID.to_be_bytes());
    assert_eq!(header.key, KEY);
    assert_eq!(header.source_hash, xxh3_128(SOURCE.as_bytes()).to_be_bytes());
    assert_eq!(header.source_size, SOURCE.len() as u64);
    assert_eq!(header.root, lowered.root());

    let read_inputs: Vec<(&[u8], u64, i64, [u8; 16])> =
        decoded.inputs.iter().map(|input| (decoded.text(input.path), input.size, input.mtime_ns, input.hash)).collect();
    assert_eq!(
        read_inputs,
        [
            (&b"src/Report.sharp"[..], 98, 1_700_000_000_123_456_789, [7; 16]),
            (&b"composer.lock"[..], 4096, -5, [9; 16]),
        ]
    );

    assert_eq!(decoded.nodes.len(), lowered.nodes().len());
    for (read, written) in decoded.nodes.iter().zip(lowered.nodes()) {
        assert_eq!(
            (read.kind, read.attr, read.line, read.end_line, read.first_child, read.child_count, read.value),
            (
                written.kind,
                written.attr,
                written.line,
                written.end_line,
                written.first_child,
                written.child_count,
                written.value
            ),
        );
        assert_eq!(
            (read.long_value, read.double_value.to_bits()),
            (written.long_value, written.double_value.to_bits())
        );
        assert_eq!(decoded.text(read.text), lowered.text(written.text));
    }

    let texts: Vec<&[u8]> =
        decoded.nodes.iter().filter(|node| node.text.len != 0).map(|node| decoded.text(node.text)).collect();
    assert_eq!(texts, [&b"strict_types"[..], b"App\\Tenant", b"weekly", b"string", b"title", b"Report"]);
    assert_eq!(decoded.children, lowered.children());
    assert_eq!(decoded.facts, FACTS);
}

#[test]
fn every_node_child_and_text_lies_inside_its_section() {
    let decoded = Decoded::new(&encoded(SOURCE));
    let nodes = decoded.nodes.len() as u64;
    let children = decoded.children.len() as u64;
    let texts = decoded.texts.len() as u64;
    let inside = |text: sharp_str| u64::from(text.offset) + u64::from(text.len) <= texts;

    assert!(u64::from(decoded.header.root) < nodes);
    for node in &decoded.nodes {
        assert!(u64::from(node.first_child) + u64::from(node.child_count) <= children, "{node:?}");
        assert!(inside(node.text), "{node:?}");
    }
    for &child in &decoded.children {
        assert!(child == u32::MAX || u64::from(child) < nodes, "child {child}");
    }
    for input in &decoded.inputs {
        assert!(inside(input.path), "{input:?}");
    }
    assert!(decoded.children.contains(&u32::MAX), "the source lowers a null child");
}

#[test]
fn every_header_field_and_section_starts_aligned_for_its_type() {
    for (field, offset, align) in [
        ("source_size", offset_of!(sharp_unit_header, source_size), align_of::<u64>()),
        ("input_count", offset_of!(sharp_unit_header, input_count), align_of::<u32>()),
        ("node_count", offset_of!(sharp_unit_header, node_count), align_of::<u32>()),
        ("children_count", offset_of!(sharp_unit_header, children_count), align_of::<u32>()),
        ("root", offset_of!(sharp_unit_header, root), align_of::<u32>()),
        ("texts_size", offset_of!(sharp_unit_header, texts_size), align_of::<u32>()),
        ("facts_size", offset_of!(sharp_unit_header, facts_size), align_of::<u32>()),
        ("sharp_input.size", offset_of!(sharp_input, size), align_of::<u64>()),
        ("sharp_input.mtime_ns", offset_of!(sharp_input, mtime_ns), align_of::<i64>()),
    ] {
        assert_eq!(offset % align, 0, "{field}");
    }

    let lowered = lowered(SOURCE);
    for count in 0..=inputs().len() {
        let decoded = Decoded::new(&encode(&lowered, SOURCE.as_bytes(), KEY, &inputs()[..count], FACTS));
        let [inputs_at, nodes_at, children_at, ..] = decoded.offsets;

        assert_eq!(inputs_at % align_of::<sharp_input>(), 0, "{count} inputs");
        assert_eq!(nodes_at % align_of::<sharp_node>(), 0, "{count} inputs");
        assert_eq!(children_at % align_of::<u32>(), 0, "{count} inputs");
    }
}

#[test]
fn encoding_one_lowering_twice_gives_the_same_bytes_with_zero_padding() {
    let first = encoded(SOURCE);
    let second = encoded(SOURCE);
    assert_eq!(first, second);

    let decoded = Decoded::new(&first);
    let after_kind = offset_of!(sharp_node, kind) + size_of::<u16>()..offset_of!(sharp_node, attr);
    let after_value = offset_of!(sharp_node, value) + size_of::<u8>()..offset_of!(sharp_node, long_value);
    for index in 0..decoded.nodes.len() {
        let node = &first[decoded.offsets[1] + index * size_of::<sharp_node>()..][..size_of::<sharp_node>()];

        assert!(node[after_kind.clone()].iter().all(|&byte| byte == 0), "node {index}");
        assert!(node[after_value.clone()].iter().all(|&byte| byte == 0), "node {index}");
    }
}

#[test]
fn header_reads_the_fields_of_a_current_file() {
    let bytes = encoded(SOURCE);
    let header = header(&bytes).unwrap();

    assert_eq!(header.abi, SHARP_UNIT_ABI);
    assert_eq!(header.checker, BUILD_ID.to_be_bytes());
    assert_eq!(header.key, KEY);
    assert_eq!(header.source_hash, source_hash(SOURCE.as_bytes()));
    assert_eq!(header.source_hash, xxh3_128(SOURCE.as_bytes()).to_be_bytes());
    assert_eq!(header.source_size, SOURCE.len() as u64);
}

#[test]
fn header_refuses_a_short_file_a_wrong_magic_a_wrong_abi_and_a_wrong_length() {
    let bytes = encoded(SOURCE);
    let changed = |offset: usize| {
        let mut changed = bytes.clone();
        changed[offset] ^= 1;
        changed
    };
    let mut longer = bytes.clone();
    longer.push(0);

    assert_eq!(header(&[]), Err(FormatError::TooShort));
    assert_eq!(header(&bytes[..size_of::<sharp_unit_header>() - 1]), Err(FormatError::TooShort));
    assert_eq!(header(&changed(0)), Err(FormatError::Magic));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, abi) + 15)), Err(FormatError::Abi));
    assert_eq!(header(&longer), Err(FormatError::Length));
    assert_eq!(header(&bytes[..bytes.len() - 1]), Err(FormatError::Length));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, node_count))), Err(FormatError::Length));
    assert_eq!(header(&changed(offset_of!(sharp_unit_header, facts_size))), Err(FormatError::Length));
}

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn stamp_gives_the_size_and_hash_of_a_file_and_none_where_no_file_is() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "app/Report.sharp", SOURCE);

    let stamped = stamp(directory.path(), b"app/Report.sharp").unwrap().expect("a file is there");

    assert_eq!(stamped.path, b"app/Report.sharp");
    assert_eq!(stamped.size, SOURCE.len() as u64);
    assert_eq!(stamped.hash, source_hash(SOURCE.as_bytes()));
    assert_ne!(stamped.mtime_ns, 0);
    assert_eq!(stamp(directory.path(), b"app/Missing.sharp").unwrap(), None);
}

#[test]
fn compiled_path_mirrors_the_real_source_path_under_the_sharp_folder() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    write(&root, "app/Shared/Money.sharp", SOURCE);

    assert_eq!(
        compiled_path(&root, &root.join("app/Shared/Money.sharp")).unwrap(),
        Some(root.join(".sharp/app/Shared/Money.sharpc"))
    );
}

#[cfg(unix)]
#[test]
fn compiled_path_is_none_for_a_link_to_a_file_outside_the_root() {
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "Money.sharp", SOURCE);
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("Money.sharp"), root.join("app/Money.sharp")).unwrap();

    assert_eq!(compiled_path(&root, &root.join("app/Money.sharp")).unwrap(), None);
}

/// A compiled file of [`SOURCE`] whose inputs are the stamps of `names` in `root`, as `mago compile` writes it.
fn compiled_with_inputs(root: &Path, names: &[&str]) -> Vec<u8> {
    let inputs: Vec<Input> = names
        .iter()
        .map(|name| {
            stamp(root, name.as_bytes()).unwrap().unwrap_or(Input {
                path: name.as_bytes().to_vec(),
                size: 0,
                mtime_ns: 0,
                hash: [0; 16],
            })
        })
        .collect();

    encode(&lowered(SOURCE), SOURCE.as_bytes(), KEY, &inputs, &[])
}

#[test]
fn a_compiled_file_is_current_until_one_of_its_inputs_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "app/Report.sharp", SOURCE);
    write(root, "app/Report.lean", "theorem a : True := trivial\n");
    let bytes = compiled_with_inputs(root, &["app/Report.sharp", "app/Report.lean", "app/Absent.lean"]);

    assert!(!is_stale(root, &bytes).unwrap(), "every input is as it was stamped");

    write(root, "app/Report.lean", "theorem a : True := by trivial\n");
    assert!(is_stale(root, &bytes).unwrap(), "an input's bytes changed");

    write(root, "app/Report.lean", "theorem a : True := trivial\n");
    assert!(!is_stale(root, &bytes).unwrap(), "rewriting the same bytes keeps it current");

    write(root, "app/Absent.lean", "theorem b : True := trivial\n");
    assert!(is_stale(root, &bytes).unwrap(), "a file appeared where an input was absent");

    fs::remove_file(root.join("app/Absent.lean")).unwrap();
    fs::remove_file(root.join("app/Report.lean")).unwrap();
    assert!(is_stale(root, &bytes).unwrap(), "an input that was there is gone");
}

#[test]
fn a_compiled_file_another_checker_build_wrote_or_that_is_no_compiled_file_is_stale() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "app/Report.sharp", SOURCE);
    let bytes = compiled_with_inputs(root, &["app/Report.sharp"]);
    let mut other_build = bytes.clone();
    other_build[offset_of!(sharp_unit_header, checker)] ^= 1;

    assert!(!is_stale(root, &bytes).unwrap());
    assert!(is_stale(root, &other_build).unwrap());
    assert!(is_stale(root, b"not a compiled file").unwrap());
}

fn read(name: &str, fingerprint: u64) -> Read {
    Read { name: name.as_bytes().to_vec(), fingerprint }
}

fn reads() -> Reads {
    Reads {
        signatures: vec![read("App\\Shared\\Money", 11), read("App\\Shared\\Money::add", 12)],
        bodies: vec![read("App\\Shared\\Money::total", 13)],
        inlined: vec![read("Sharp\\List::count", 31)],
        listed: vec![read("class-likes", 41)],
    }
}

#[test]
fn changing_only_an_inlined_body_changes_the_key() {
    let source = source_hash(b"namespace App.Orders;");
    let mut changed = reads();
    changed.inlined[0].fingerprint = xxhash_rust::xxh3::xxh3_64(b"return \\count($this->items);");

    assert_ne!(key(source, &changed), key(source, &reads()));
}

#[test]
fn the_key_changes_with_the_source_and_with_each_read() {
    let source = source_hash(b"namespace App.Orders;");
    let base = key(source, &reads());
    assert_eq!(base, key(source, &reads()));

    let mut changes: Vec<(&str, [u8; 16], Reads)> = vec![("the source", source_hash(b"namespace App.Bills;"), reads())];
    for (change, edit) in [
        ("a signature's fingerprint", (|reads: &mut Reads| reads.signatures[0].fingerprint = 21) as fn(&mut Reads)),
        ("a signature's name", |reads| reads.signatures[0].name = b"App\\Shared\\Price".to_vec()),
        ("one more signature", |reads| reads.signatures.push(read("App\\Shared\\Currency", 14))),
        ("one signature fewer", |reads| drop(reads.signatures.pop())),
        ("an inferred return", |reads| reads.bodies[0].fingerprint = 23),
        ("a body's name", |reads| reads.bodies[0].name = b"App\\Shared\\Money::sum".to_vec()),
        ("a read moved between the lists", |reads| {
            let moved = reads.signatures.remove(1);
            reads.bodies.push(moved);
        }),
        ("a listing's answer", |reads| reads.listed[0].fingerprint = 42),
        ("one more listing", |reads| reads.listed.push(read("functions", 43))),
        ("an inlined form moved to the listings", |reads| {
            let moved = reads.inlined.remove(0);
            reads.listed.push(moved);
        }),
    ] {
        let mut edited = reads();
        edit(&mut edited);
        changes.push((change, source, edited));
    }

    for (change, source, reads) in changes {
        assert_ne!(key(source, &reads), base, "{change}");
    }
}

#[test]
fn the_key_ignores_the_order_and_repeats_of_reads() {
    let source = source_hash(b"namespace App.Orders;");
    let mut shuffled = reads();
    shuffled.signatures.reverse();
    shuffled.signatures.push(read("App\\Shared\\Money", 11));
    shuffled.bodies.push(read("App\\Shared\\Money::total", 13));

    assert_eq!(key(source, &shuffled), key(source, &reads()));
}

#[test]
fn a_body_edit_keeps_the_key_until_the_inferred_return_changes() {
    let source = source_hash(b"namespace App.Orders;");
    let total = |inferred_return: &str| Reads {
        signatures: vec![read("App\\Shared\\Money", 11)],
        bodies: vec![read("App\\Shared\\Money::total", xxhash_rust::xxh3::xxh3_64(inferred_return.as_bytes()))],
        inlined: Vec::new(),
        listed: Vec::new(),
    };
    let before = key(source, &total("int"));

    assert_eq!(key(source, &total("int")), before, "the body changed and still returns int");
    assert_ne!(key(source, &total("float")), before, "the body now returns float");
}

/// The C header cbindgen writes for `ext/sharp`.
const HEADER: &str = include_str!(concat!(env!("OUT_DIR"), "/sharp_unit.h"));

#[test]
#[cfg(unix)]
fn the_c_header_lays_out_every_type_as_the_bridge_does() {
    let check = tempfile::tempdir().unwrap();
    fs::write(check.path().join("sharp_unit.h"), HEADER).unwrap();
    let assertions = [
        ("sizeof(sharp_unit_header)", size_of::<sharp_unit_header>()),
        ("offsetof(sharp_unit_header, abi)", offset_of!(sharp_unit_header, abi)),
        ("offsetof(sharp_unit_header, checker)", offset_of!(sharp_unit_header, checker)),
        ("offsetof(sharp_unit_header, key)", offset_of!(sharp_unit_header, key)),
        ("offsetof(sharp_unit_header, source_hash)", offset_of!(sharp_unit_header, source_hash)),
        ("offsetof(sharp_unit_header, source_size)", offset_of!(sharp_unit_header, source_size)),
        ("offsetof(sharp_unit_header, input_count)", offset_of!(sharp_unit_header, input_count)),
        ("offsetof(sharp_unit_header, node_count)", offset_of!(sharp_unit_header, node_count)),
        ("offsetof(sharp_unit_header, children_count)", offset_of!(sharp_unit_header, children_count)),
        ("offsetof(sharp_unit_header, root)", offset_of!(sharp_unit_header, root)),
        ("offsetof(sharp_unit_header, texts_size)", offset_of!(sharp_unit_header, texts_size)),
        ("offsetof(sharp_unit_header, facts_size)", offset_of!(sharp_unit_header, facts_size)),
        ("sizeof(sharp_input)", size_of::<sharp_input>()),
        ("offsetof(sharp_input, size)", offset_of!(sharp_input, size)),
        ("offsetof(sharp_input, mtime_ns)", offset_of!(sharp_input, mtime_ns)),
        ("offsetof(sharp_input, hash)", offset_of!(sharp_input, hash)),
        ("sizeof(sharp_node)", size_of::<sharp_node>()),
        ("offsetof(sharp_node, attr)", offset_of!(sharp_node, attr)),
        ("offsetof(sharp_node, value)", offset_of!(sharp_node, value)),
        ("offsetof(sharp_node, long_value)", offset_of!(sharp_node, long_value)),
        ("offsetof(sharp_node, text)", offset_of!(sharp_node, text)),
        ("sizeof(sharp_str)", size_of::<sharp_str>()),
        ("sizeof(SHARP_UNIT_MAGIC) - 1", SHARP_UNIT_MAGIC.len()),
        ("sizeof(SHARP_UNIT_ABI) - 1", SHARP_UNIT_ABI.len()),
        ("sizeof(SHARP_MAGO_COMMIT) - 1", 40),
    ];
    let mut source = String::from("#include <stddef.h>\n#include \"sharp_unit.h\"\n");
    for (expression, value) in assertions {
        let _ = writeln!(source, "_Static_assert({expression} == {value}, \"{expression}\");");
    }
    fs::write(check.path().join("check.c"), source).unwrap();

    let output = Command::new("cc")
        .args(["-std=c11", "-fsyntax-only", "-Werror"])
        .arg(check.path().join("check.c"))
        .output()
        .expect("cc runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn the_c_header_carries_the_magic_the_abi_and_the_mago_commit() {
    let define = |name: &str| {
        HEADER
            .lines()
            .find_map(|line| line.strip_prefix(&format!("#define {name} ")))
            .unwrap_or_else(|| panic!("the header defines {name}"))
            .to_owned()
    };
    let bytes = |bytes: &[u8]| -> String {
        let mut literal = String::from("\"");
        for byte in bytes {
            let _ = write!(literal, "\\x{byte:02x}");
        }
        literal.push('"');

        literal
    };
    let commit = String::from_utf8(
        Command::new("git").args(["rev-parse", "HEAD"]).current_dir(repository()).output().unwrap().stdout,
    )
    .unwrap();

    assert_eq!(define("SHARP_UNIT_MAGIC"), bytes(b"SHARPC\0\0"));
    assert_eq!(define("SHARP_UNIT_ABI"), bytes(&SHARP_UNIT_ABI));
    assert_eq!(define("SHARP_MAGO_COMMIT"), format!("\"{}\"", commit.trim()));
    assert!(HEADER.contains("#error"), "the header refuses a big-endian target");
}

#[test]
fn the_c_header_defines_each_token_the_lowering_writes() {
    let kinds = fs::read_to_string(repository().join("crates/sharp-bridge/src/kind.rs")).unwrap();
    let tokens: Vec<(&str, &str)> = kinds
        .lines()
        .filter_map(|line| line.strip_prefix("pub const SHARP_T_")?.strip_suffix(';')?.split_once(": u32 = "))
        .collect();

    assert!(!tokens.is_empty(), "kind.rs holds the tokens the lowering writes");
    for (token, value) in tokens {
        assert!(HEADER.contains(&format!("\n#define SHARP_T_{token} {value}\n")), "SHARP_T_{token}: {HEADER}");
    }
}

fn repository() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Whether `php` runs. `MAGO_REQUIRE_PHP_SDK_TESTS` makes its absence a failure, as in Mago's PHP SDK tests.
fn php_is_available(test: &str) -> bool {
    let available = Command::new("php").arg("--version").output().is_ok_and(|output| output.status.success());
    assert!(available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(), "PHP is required for {test}");

    available
}

const ZEND_AST_H: &str = "#define ZEND_AST_SPECIAL_SHIFT      6
#define ZEND_AST_IS_LIST_SHIFT      7

enum _zend_ast_kind {
\t/* special nodes */
\tZEND_AST_ZVAL = 1 << ZEND_AST_SPECIAL_SHIFT,
\tZEND_AST_CONSTANT,

\t/* list nodes */
\tZEND_AST_ARG_LIST = 1 << ZEND_AST_IS_LIST_SHIFT,
};
";

const ZEND_LANGUAGE_PARSER_H: &str = "  enum zendtokentype
  {
    ZENDEMPTY = -2,
    END = 0,                       /* \"end of file\"  */
    T_LINE = 346,                  /* \"'__LINE__'\"  */
    T_FILE = 347,                  /* \"'__FILE__'\"  */
    T_DIR = 348,                   /* \"'__DIR__'\"  */
  };
";

/// The generator given only `zend_ast`, run in a copy of the repository's script and bridge sources after `edit`
/// changes one of those sources. The copy lives as long as the returned folder.
fn generator(zend_ast: &str, edit: Option<(&str, &str, &str)>) -> (tempfile::TempDir, Command) {
    let root = tempfile::tempdir().unwrap();
    let sources = root.path().join("crates/sharp-bridge/src");
    fs::create_dir_all(root.path().join("scripts")).unwrap();
    fs::create_dir_all(&sources).unwrap();
    fs::copy(repository().join("scripts/regen-sharp-kinds.php"), root.path().join("scripts/regen-sharp-kinds.php"))
        .unwrap();
    for file in ["lib.rs", "unit.rs"] {
        fs::copy(repository().join("crates/sharp-bridge/src").join(file), sources.join(file)).unwrap();
    }
    if let Some((file, from, to)) = edit {
        let source = fs::read_to_string(sources.join(file)).unwrap();
        assert!(source.contains(from), "{file} contains `{from}`");
        fs::write(sources.join(file), source.replacen(from, to, 1)).unwrap();
    }
    fs::write(root.path().join("zend_ast.h"), zend_ast).unwrap();

    let mut command = Command::new("php");
    command.arg(root.path().join("scripts/regen-sharp-kinds.php")).arg(root.path().join("zend_ast.h"));

    (root, command)
}

/// What the generator prints for `zend_ast` and `zend_language_parser` after `edit`.
fn generated(zend_ast: &str, zend_language_parser: &str, edit: Option<(&str, &str, &str)>) -> String {
    let (root, mut command) = generator(zend_ast, edit);
    fs::write(root.path().join("zend_language_parser.h"), zend_language_parser).unwrap();
    let output = command.arg(root.path().join("zend_language_parser.h")).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    String::from_utf8(output.stdout).unwrap()
}

/// The `SHARP_UNIT_ABI` line the generator prints for `zend_ast` and `zend_language_parser` after `edit`.
fn generated_abi(zend_ast: &str, zend_language_parser: &str, edit: Option<(&str, &str, &str)>) -> String {
    generated(zend_ast, zend_language_parser, edit)
        .lines()
        .find(|line| line.starts_with("pub const SHARP_UNIT_ABI: [u8; 16] = 0x"))
        .expect("the generator prints SHARP_UNIT_ABI")
        .to_owned()
}

#[test]
fn the_unit_abi_changes_with_the_kind_table_the_tokens_and_the_layouts() {
    if !php_is_available("the_unit_abi_changes_with_the_kind_table_the_tokens_and_the_layouts") {
        return;
    }

    let base = generated_abi(ZEND_AST_H, ZEND_LANGUAGE_PARSER_H, None);
    assert_eq!(generated_abi(ZEND_AST_H, ZEND_LANGUAGE_PARSER_H, None), base);
    assert_eq!(
        generated_abi(ZEND_AST_H, ZEND_LANGUAGE_PARSER_H, Some(("lib.rs", "/// First line.", "/// The first line."))),
        base,
        "a comment is not layout"
    );
    assert_eq!(
        generated_abi(ZEND_AST_H, &ZEND_LANGUAGE_PARSER_H.replace("T_LINE = 346", "T_LINE = 345"), None),
        base,
        "a token the lowering does not write is not ABI"
    );

    for (change, abi) in [
        (
            "a kind",
            generated_abi(
                &ZEND_AST_H.replace("ZEND_AST_CONSTANT,", "ZEND_AST_CONSTANT,\n\tZEND_AST_ZNODE,"),
                ZEND_LANGUAGE_PARSER_H,
                None,
            ),
        ),
        ("a token", generated_abi(ZEND_AST_H, &ZEND_LANGUAGE_PARSER_H.replace("T_FILE = 347", "T_FILE = 348"), None)),
        (
            "a sharp_node field",
            generated_abi(ZEND_AST_H, ZEND_LANGUAGE_PARSER_H, Some(("lib.rs", "pub line: u32,", "pub line: u64,"))),
        ),
        (
            "a sharp_value case",
            generated_abi(
                ZEND_AST_H,
                ZEND_LANGUAGE_PARSER_H,
                Some(("lib.rs", "SHARP_LONG,", "SHARP_LONG,\n    SHARP_ARRAY,")),
            ),
        ),
        (
            "a sharp_str field",
            generated_abi(ZEND_AST_H, ZEND_LANGUAGE_PARSER_H, Some(("lib.rs", "pub len: u32,", "pub len: u64,"))),
        ),
        (
            "a header field",
            generated_abi(
                ZEND_AST_H,
                ZEND_LANGUAGE_PARSER_H,
                Some(("unit.rs", "pub facts_size: u32,", "pub facts_size: u64,")),
            ),
        ),
        (
            "a sharp_input field",
            generated_abi(
                ZEND_AST_H,
                ZEND_LANGUAGE_PARSER_H,
                Some(("unit.rs", "pub mtime_ns: i64,", "pub mtime_ns: u64,")),
            ),
        ),
    ] {
        assert_ne!(abi, base, "{change}");
    }
}

#[test]
fn each_generated_token_equals_its_value_in_zend_language_parser_h() {
    if !php_is_available("each_generated_token_equals_its_value_in_zend_language_parser_h") {
        return;
    }

    for value in ["347", "512"] {
        let kinds =
            generated(ZEND_AST_H, &ZEND_LANGUAGE_PARSER_H.replace("T_FILE = 347", &format!("T_FILE = {value}")), None);

        assert!(kinds.contains(&format!("\npub const SHARP_T_FILE: u32 = {value};\n")), "{kinds}");
    }
}

#[test]
fn the_kind_generator_requires_zend_language_parser_h() {
    if !php_is_available("the_kind_generator_requires_zend_language_parser_h") {
        return;
    }

    let (_root, mut command) = generator(ZEND_AST_H, None);
    let output = command.output().unwrap();

    assert!(!output.status.success(), "a missing parser header is an error");
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(printed.contains("Pass the path to php-sharp's `Zend/zend_language_parser.h`."), "{printed}");
}

/// The generator reads two headers, so a third argument is an error.
#[test]
fn the_kind_generator_takes_only_zend_ast_h_and_zend_language_parser_h() {
    if !php_is_available("the_kind_generator_takes_only_zend_ast_h_and_zend_language_parser_h") {
        return;
    }

    let (root, mut command) = generator(ZEND_AST_H, None);
    fs::write(root.path().join("zend_language_parser.h"), ZEND_LANGUAGE_PARSER_H).unwrap();
    let output = command
        .arg(root.path().join("zend_language_parser.h"))
        .arg(root.path().join("zend_language_parser.h"))
        .output()
        .unwrap();

    assert!(!output.status.success(), "a third argument is an error");
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(
        printed.contains(
            "Pass only the paths to php-src's `Zend/zend_ast.h` and php-sharp's `Zend/zend_language_parser.h`."
        ),
        "{printed}"
    );
}

#[test]
fn the_kind_generator_refuses_a_parser_header_without_a_token_the_lowering_writes() {
    if !php_is_available("the_kind_generator_refuses_a_parser_header_without_a_token_the_lowering_writes") {
        return;
    }

    let (root, mut command) = generator(ZEND_AST_H, None);
    fs::write(root.path().join("zend_language_parser.h"), ZEND_LANGUAGE_PARSER_H.replace("T_FILE", "T_PATH")).unwrap();
    let output = command.arg(root.path().join("zend_language_parser.h")).output().unwrap();

    assert!(!output.status.success(), "a parser header without T_FILE is an error");
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(printed.contains("Unable to find the token `T_FILE`"), "{printed}");
}

/// The lowering names a php-src token only by its `SHARP_T_` constant, which the generator writes to `kind.rs`, so a
/// token the generator does not write is a compile error.
#[test]
fn the_bridge_names_each_token_by_its_generated_constant() {
    let mut named = Vec::new();
    let mut folders = vec![repository().join("crates/sharp-bridge/src")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else if !path.ends_with("kind.rs") {
                let source = fs::read_to_string(&path).unwrap();
                for word in source.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_')) {
                    if word.starts_with("T_") {
                        named.push(format!("{} names {word}", path.display()));
                    }
                }
            }
        }
    }

    assert!(named.is_empty(), "{named:#?}");
}
