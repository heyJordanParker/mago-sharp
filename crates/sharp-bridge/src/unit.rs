//! The `.sharpc` compiled file the engine runs.
//!
//! A file is a header, then the inputs, the tree, the texts and the facts, each section starting where the one before
//! it ends. `ext/sharp` reads a file by casting its bytes, so every field and section is aligned for its type, padding
//! is zero, and the file is little-endian.

#![allow(clippy::big_endian_bytes, clippy::little_endian_bytes)]

use std::io;
use std::mem::offset_of;
use std::mem::size_of;
use std::path::Path;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use mago_build_id::BUILD_ID;
use xxhash_rust::xxh3::Xxh3;
use xxhash_rust::xxh3::xxh3_128;

use crate::SHARP_UNIT_ABI;
use crate::Unit;
use crate::sharp_node;
use crate::sharp_str;
use crate::store_text;

#[cfg(target_endian = "big")]
compile_error!("a .sharpc file is little-endian, and the engine reads it by casting its bytes");

pub const SHARP_UNIT_MAGIC: [u8; 8] = *b"SHARPC\0\0";

/// The folder at the workspace root, and at each package root, that holds every compiled file.
pub const COMPILED_FOLDER: &str = ".sharp";

/// The first bytes of a `.sharpc` file. Every 16-byte hash is xxh3-128 in canonical big-endian order.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_unit_header {
    /// `SHARP_UNIT_MAGIC`.
    pub magic: [u8; 8],
    /// `SHARP_UNIT_ABI` of the checker that wrote the file.
    pub abi: [u8; 16],
    /// The build ID of the checker that wrote the file.
    pub checker: [u8; 16],
    /// Names the compiled code.
    pub key: [u8; 16],
    pub source_hash: [u8; 16],
    pub source_size: u64,
    pub input_count: u32,
    pub node_count: u32,
    pub children_count: u32,
    /// A `SHARP_AST_STMT_LIST`.
    pub root: u32,
    pub texts_size: u32,
    pub facts_size: u32,
}

/// A file whose edit makes the compiled file's source due for a check.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_input {
    /// Workspace-relative, with `/` separators.
    pub path: sharp_str,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: [u8; 16],
}

const _: () = {
    assert!(size_of::<sharp_unit_header>() == 104, "the header is 104 bytes");
    assert!(offset_of!(sharp_unit_header, abi) == 8, "abi is at 8");
    assert!(offset_of!(sharp_unit_header, checker) == 24, "checker is at 24");
    assert!(offset_of!(sharp_unit_header, key) == 40, "key is at 40");
    assert!(offset_of!(sharp_unit_header, source_hash) == 56, "source_hash is at 56");
    assert!(offset_of!(sharp_unit_header, source_size) == 72, "source_size is at 72");
    assert!(offset_of!(sharp_unit_header, input_count) == 80, "input_count is at 80");
    assert!(offset_of!(sharp_unit_header, node_count) == 84, "node_count is at 84");
    assert!(offset_of!(sharp_unit_header, children_count) == 88, "children_count is at 88");
    assert!(offset_of!(sharp_unit_header, root) == 92, "root is at 92");
    assert!(offset_of!(sharp_unit_header, texts_size) == 96, "texts_size is at 96");
    assert!(offset_of!(sharp_unit_header, facts_size) == 100, "facts_size is at 100");
    assert!(size_of::<sharp_input>() == 40, "an input is 40 bytes");
    assert!(offset_of!(sharp_input, size) == 8, "an input's size is at 8");
    assert!(offset_of!(sharp_input, mtime_ns) == 16, "an input's mtime_ns is at 16");
    assert!(offset_of!(sharp_input, hash) == 24, "an input's hash is at 24");
    assert!(size_of::<sharp_node>() == 56, "a node is 56 bytes");
};

/// An input before the encoder stores its path in the texts.
///
/// An input no file was at when it was stamped is absent: its size, modification time and hash are all zero. A file
/// that exists never has that stamp, because the hash of an empty file is not zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// Workspace-relative, with `/` separators.
    pub path: Vec<u8>,
    pub size: u64,
    pub mtime_ns: i64,
    pub hash: [u8; 16],
}

/// One declaration a check read, and the fingerprint of what it read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Read {
    pub name: Vec<u8>,
    pub fingerprint: u64,
}

/// What a file's check, and the extensions that analyzed it, read from other declarations. Only these enter the key,
/// so an edit that changes none of them leaves the compiled code's name unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reads {
    /// Each declaration the check read through its signature, and each declaration an extension read with its
    /// ancestors, fingerprinted by codex's `DefSignatureNode::signature_hash`.
    pub signatures: Vec<Read>,
    /// Each method whose inferred return the check read, named `Class::method` and fingerprinted by the xxh3-64 of
    /// that return type's `TUnion::get_id()`.
    pub bodies: Vec<Read>,
    /// Each library method whose body the file inlines, named `Class::method` and fingerprinted by the xxh3-64 of the
    /// inlined form's bytes.
    pub inlined: Vec<Read>,
    /// Each set of names an extension listed while it analyzed the file, fingerprinted by a hash of the names the set
    /// holds, so a name joining or leaving it changes the key.
    pub listed: Vec<Read>,
}

/// The fields of a header that passed load checks 1 and 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub abi: [u8; 16],
    pub checker: [u8; 16],
    pub key: [u8; 16],
    pub source_hash: [u8; 16],
    pub source_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// The file ends before its header does.
    TooShort,
    /// The file does not start with `SHARP_UNIT_MAGIC`.
    Magic,
    /// The file was written for a different `SHARP_UNIT_ABI`.
    Abi,
    /// The file's length differs from the length its header gives.
    Length,
}

/// The xxh3-128 of a source, as the header's `source_hash` stores it.
#[must_use]
pub fn source_hash(source: &[u8]) -> [u8; 16] {
    xxh3_128(source).to_be_bytes()
}

/// The name of the code a check compiles: the hash of the magic, `SHARP_UNIT_ABI`, the checker's build ID, the
/// source's hash and the reads. The order the reads come in never changes it.
#[must_use]
pub fn key(source_hash: [u8; 16], reads: &Reads) -> [u8; 16] {
    key_of(&[&SHARP_UNIT_MAGIC, &SHARP_UNIT_ABI, &BUILD_ID.to_be_bytes()], source_hash, reads)
}

fn key_of(compiler: &[&[u8]], source_hash: [u8; 16], reads: &Reads) -> [u8; 16] {
    let mut hasher = Xxh3::new();
    for part in compiler {
        hasher.update(part);
    }
    hasher.update(&source_hash);

    for list in [&reads.signatures, &reads.bodies, &reads.inlined, &reads.listed] {
        let mut list: Vec<&Read> = list.iter().collect();
        list.sort();
        list.dedup();

        hasher.update(&size("reads", list.len()).to_le_bytes());
        for read in list {
            hasher.update(&size("a read's name", read.name.len()).to_le_bytes());
            hasher.update(&read.name);
            hasher.update(&read.fingerprint.to_le_bytes());
        }
    }

    hasher.digest128().to_be_bytes()
}

/// The bytes of the `.sharpc` file for `unit`, lowered from `source`. The same arguments always give the same bytes.
///
/// # Panics
///
/// Panics when a section holds more than a 32-bit size can say.
#[must_use]
pub fn encode(unit: &Unit, source: &[u8], key: [u8; 16], inputs: &[Input], facts: &[u8]) -> Vec<u8> {
    let mut texts = unit.texts.clone();
    let inputs: Vec<sharp_input> = inputs
        .iter()
        .map(|input| sharp_input {
            path: store_text(&mut texts, &input.path),
            size: input.size,
            mtime_ns: input.mtime_ns,
            hash: input.hash,
        })
        .collect();

    let header = sharp_unit_header {
        magic: SHARP_UNIT_MAGIC,
        abi: SHARP_UNIT_ABI,
        checker: BUILD_ID.to_be_bytes(),
        key,
        source_hash: source_hash(source),
        source_size: source.len() as u64,
        input_count: size("inputs", inputs.len()),
        node_count: size("nodes", unit.nodes.len()),
        children_count: size("children", unit.children.len()),
        root: unit.root,
        texts_size: size("texts", texts.len()),
        facts_size: size("facts", facts.len()),
    };

    let mut bytes = Vec::with_capacity(length(&header) as usize);
    bytes.extend_from_slice(&header_bytes(&header));
    for input in &inputs {
        bytes.extend_from_slice(&input_bytes(input));
    }
    for node in &unit.nodes {
        bytes.extend_from_slice(&node_bytes(node));
    }
    for child in &unit.children {
        bytes.extend_from_slice(&child.to_le_bytes());
    }
    bytes.extend_from_slice(&texts);
    bytes.extend_from_slice(facts);

    bytes
}

/// Reads the header of a `.sharpc` file, after load checks 1 and 2: the magic and the ABI match this bridge's, and the
/// file's length is the length its header gives.
///
/// # Errors
///
/// Returns the first check the file fails.
pub fn header(bytes: &[u8]) -> Result<Header, FormatError> {
    let Some(fixed) = bytes.get(..size_of::<sharp_unit_header>()) else {
        return Err(FormatError::TooShort);
    };
    if fixed[..SHARP_UNIT_MAGIC.len()] != SHARP_UNIT_MAGIC {
        return Err(FormatError::Magic);
    }

    let hash = |offset: usize| -> [u8; 16] {
        let mut hash = [0; 16];
        hash.copy_from_slice(&fixed[offset..offset + 16]);
        hash
    };
    let word = |offset: usize| -> u32 {
        let mut word = [0; 4];
        word.copy_from_slice(&fixed[offset..offset + 4]);
        u32::from_le_bytes(word)
    };
    let mut source_size = [0; 8];
    source_size.copy_from_slice(&fixed[offset_of!(sharp_unit_header, source_size)..][..8]);

    let header = sharp_unit_header {
        magic: SHARP_UNIT_MAGIC,
        abi: hash(offset_of!(sharp_unit_header, abi)),
        checker: hash(offset_of!(sharp_unit_header, checker)),
        key: hash(offset_of!(sharp_unit_header, key)),
        source_hash: hash(offset_of!(sharp_unit_header, source_hash)),
        source_size: u64::from_le_bytes(source_size),
        input_count: word(offset_of!(sharp_unit_header, input_count)),
        node_count: word(offset_of!(sharp_unit_header, node_count)),
        children_count: word(offset_of!(sharp_unit_header, children_count)),
        root: word(offset_of!(sharp_unit_header, root)),
        texts_size: word(offset_of!(sharp_unit_header, texts_size)),
        facts_size: word(offset_of!(sharp_unit_header, facts_size)),
    };
    if header.abi != SHARP_UNIT_ABI {
        return Err(FormatError::Abi);
    }
    if bytes.len() as u64 != length(&header) {
        return Err(FormatError::Length);
    }

    Ok(Header {
        abi: header.abi,
        checker: header.checker,
        key: header.key,
        source_hash: header.source_hash,
        source_size: header.source_size,
    })
}

/// The size, modification time and hash of the file at the workspace-relative `path` under `root`, or none when no
/// file is there.
///
/// # Errors
///
/// Returns the error of reading the file, other than its absence.
pub fn stamp(root: &Path, path: &[u8]) -> io::Result<Option<Input>> {
    let full = root.join(String::from_utf8_lossy(path).as_ref());
    let contents = match std::fs::read(&full) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mtime_ns = match std::fs::metadata(&full)?.modified()?.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |nanos| -nanos),
    };

    Ok(Some(Input { path: path.to_vec(), size: contents.len() as u64, mtime_ns, hash: source_hash(&contents) }))
}

/// Where the compiled file of `source` lives, or none when `source` is a link to a file outside `root`.
///
/// The compiled file sits in the `.sharp` folder at the canonical `root`, at the real path of `source` relative to
/// `root`, with `.sharp` replaced by `.sharpc`.
///
/// # Errors
///
/// Returns the error of resolving the real path of `source`.
pub fn compiled_path(root: &Path, source: &Path) -> io::Result<Option<PathBuf>> {
    let real = source.canonicalize()?;

    Ok(real.strip_prefix(root).ok().map(|relative| root.join(COMPILED_FOLDER).join(relative).with_extension("sharpc")))
}

/// Returns `true` when the compiled file `bytes` cannot stand for its source any more.
///
/// That is when it is no compiled file this bridge reads, another checker build wrote it, or one of its inputs under
/// `root` differs from its stamp. An input differs when its size or hash changed, when it is gone, or when a file
/// appeared where it was absent.
///
/// # Errors
///
/// Returns the error of reading an input.
pub fn is_stale(root: &Path, bytes: &[u8]) -> io::Result<bool> {
    let Ok(header) = header(bytes) else {
        return Ok(true);
    };
    if header.checker != BUILD_ID.to_be_bytes() {
        return Ok(true);
    }

    let word = |offset: usize| -> usize {
        let mut word = [0; 4];
        word.copy_from_slice(&bytes[offset..offset + 4]);
        u32::from_le_bytes(word) as usize
    };
    let count = word(offset_of!(sharp_unit_header, input_count));
    let texts = size_of::<sharp_unit_header>()
        + size_of::<sharp_input>() * count
        + size_of::<sharp_node>() * word(offset_of!(sharp_unit_header, node_count))
        + size_of::<u32>() * word(offset_of!(sharp_unit_header, children_count));
    for index in 0..count {
        let input = size_of::<sharp_unit_header>() + size_of::<sharp_input>() * index;
        let path_at = input + offset_of!(sharp_input, path);
        let path = &bytes[texts + word(path_at + offset_of!(sharp_str, offset))..]
            [..word(path_at + offset_of!(sharp_str, len))];
        let mut size = [0; 8];
        size.copy_from_slice(&bytes[input + offset_of!(sharp_input, size)..][..8]);
        let hash = &bytes[input + offset_of!(sharp_input, hash)..][..16];

        let current = match stamp(root, path)? {
            Some(stamped) => (stamped.size, stamped.hash),
            None => (0, [0; 16]),
        };
        if current != (u64::from_le_bytes(size), hash.try_into().unwrap_or([0; 16])) {
            return Ok(true);
        }
    }

    Ok(false)
}

/// The length of the file `header` describes.
fn length(header: &sharp_unit_header) -> u64 {
    size_of::<sharp_unit_header>() as u64
        + size_of::<sharp_input>() as u64 * u64::from(header.input_count)
        + size_of::<sharp_node>() as u64 * u64::from(header.node_count)
        + size_of::<u32>() as u64 * u64::from(header.children_count)
        + u64::from(header.texts_size)
        + u64::from(header.facts_size)
}

/// A section's size, which the header stores in 32 bits.
fn size(section: &str, size: usize) -> u32 {
    u32::try_from(size).unwrap_or_else(|_| panic!("{size} {section} exceed the 32-bit size a compiled file stores"))
}

fn header_bytes(header: &sharp_unit_header) -> [u8; size_of::<sharp_unit_header>()] {
    let mut bytes = [0; size_of::<sharp_unit_header>()];
    put(&mut bytes, offset_of!(sharp_unit_header, magic), &header.magic);
    put(&mut bytes, offset_of!(sharp_unit_header, abi), &header.abi);
    put(&mut bytes, offset_of!(sharp_unit_header, checker), &header.checker);
    put(&mut bytes, offset_of!(sharp_unit_header, key), &header.key);
    put(&mut bytes, offset_of!(sharp_unit_header, source_hash), &header.source_hash);
    put(&mut bytes, offset_of!(sharp_unit_header, source_size), &header.source_size.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, input_count), &header.input_count.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, node_count), &header.node_count.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, children_count), &header.children_count.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, root), &header.root.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, texts_size), &header.texts_size.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_unit_header, facts_size), &header.facts_size.to_le_bytes());

    bytes
}

fn input_bytes(input: &sharp_input) -> [u8; size_of::<sharp_input>()] {
    let mut bytes = [0; size_of::<sharp_input>()];
    put_text(&mut bytes, offset_of!(sharp_input, path), input.path);
    put(&mut bytes, offset_of!(sharp_input, size), &input.size.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_input, mtime_ns), &input.mtime_ns.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_input, hash), &input.hash);

    bytes
}

/// A node's fields at their offsets, with its padding zero.
/// The xxh3-64 of an inline form's nodes, children and texts, each written as `encode` writes them.
pub(crate) fn form_fingerprint(nodes: &[sharp_node], children: &[u32], texts: &[u8]) -> u64 {
    let mut hasher = Xxh3::new();
    for node in nodes {
        hasher.update(&node_bytes(node));
    }
    for child in children {
        hasher.update(&child.to_le_bytes());
    }
    hasher.update(texts);

    hasher.digest()
}

fn node_bytes(node: &sharp_node) -> [u8; size_of::<sharp_node>()] {
    let mut bytes = [0; size_of::<sharp_node>()];
    put(&mut bytes, offset_of!(sharp_node, kind), &(node.kind as u16).to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, attr), &node.attr.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, line), &node.line.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, end_line), &node.end_line.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, first_child), &node.first_child.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, child_count), &node.child_count.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, value), &[node.value as u8]);
    put(&mut bytes, offset_of!(sharp_node, long_value), &node.long_value.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_node, double_value), &node.double_value.to_le_bytes());
    put_text(&mut bytes, offset_of!(sharp_node, text), node.text);

    bytes
}

fn put_text(bytes: &mut [u8], offset: usize, text: sharp_str) {
    put(bytes, offset + offset_of!(sharp_str, offset), &text.offset.to_le_bytes());
    put(bytes, offset + offset_of!(sharp_str, len), &text.len.to_le_bytes());
}

fn put(bytes: &mut [u8], offset: usize, field: &[u8]) {
    bytes[offset..offset + field.len()].copy_from_slice(field);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_changes_with_each_part_of_the_compiler() {
        let compiler: [&[u8]; 3] = [b"magic", b"abi", b"checker"];
        let base = key_of(&compiler, [0; 16], &Reads::default());

        for index in 0..compiler.len() {
            let mut changed = compiler;
            changed[index] = b"changed";

            assert_ne!(key_of(&changed, [0; 16], &Reads::default()), base, "part {index}");
        }
    }
}
