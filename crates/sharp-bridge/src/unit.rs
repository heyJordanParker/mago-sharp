//! The `.sharpc` compiled file the engine runs.
//!
//! A file is a header, then the inputs, the tree and the texts, each section starting where the one before it ends.
//! `ext/sharp` reads a file by casting its bytes, so every field and section is aligned for its type, padding is zero,
//! and the file is little-endian.

#![allow(clippy::big_endian_bytes, clippy::little_endian_bytes)]

use std::io;
use std::mem::offset_of;
use std::mem::size_of;
use std::path::Path;
use std::path::PathBuf;

use mago_codex::signature::FileSignature;
use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::file::File;
use mago_database::file::nanos_since_epoch;
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

/// The last segment of an input that stands for the `.sharp` files of its folder, as in `app/Shop/*.sharp`.
///
/// Its stamp is the listing of the names of the regular files whose name ends in `.sharp` after at least one other
/// byte, and of the folders whose name starts with an ASCII uppercase letter, as a namespace part does, each with a `/`
/// after it, each followed by a newline, in byte order: its size is the listing's length, its modification time zero,
/// and its hash the listing's. A link counts as what it reaches, and a link that reaches nothing is left out. A file or
/// a namespace folder appearing or going changes it, and an edit doesn't.
pub const SOURCE_LISTING: &str = "*.sharp";

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
    /// The native fingerprint of the standard library the file was compiled against, which the engine compares with
    /// its own `SHARP_NATIVE`. All zero when the project holds no standard library.
    pub native_fingerprint: [u8; 16],
    /// The version Composer installed that standard library at, which the engine's refusal names. Empty when Composer
    /// reports none.
    pub library_version: sharp_str,
    /// The hash of every byte after the header: the inputs, the tree and the texts. The engine refuses a file whose
    /// sections changed after it was written, as a damaged or partly written one.
    pub sections_hash: [u8; 16],
    /// The hash of the file's declarations, as [`declarations_hash`] gives it, which each file that reads this one
    /// records and the engine compares instead of the source's stamp.
    pub declarations_hash: [u8; 16],
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
    /// For a `.sharp` file of the project whose declarations or bodies the compiled file reads, the
    /// `declarations_hash` of that file's compiled file, so an edit that changes none of its declarations leaves the
    /// input current. All zero for every other input, which holds while its stamp does.
    pub declarations_hash: [u8; 16],
}

const _: () = {
    assert!(size_of::<sharp_unit_header>() == 160, "the header is 160 bytes");
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
    assert!(offset_of!(sharp_unit_header, native_fingerprint) == 100, "native_fingerprint is at 100");
    assert!(offset_of!(sharp_unit_header, library_version) == 116, "library_version is at 116");
    assert!(offset_of!(sharp_unit_header, sections_hash) == 124, "sections_hash is at 124");
    assert!(offset_of!(sharp_unit_header, declarations_hash) == 140, "declarations_hash is at 140");
    assert!(size_of::<sharp_input>() == 56, "an input is 56 bytes");
    assert!(offset_of!(sharp_input, size) == 8, "an input's size is at 8");
    assert!(offset_of!(sharp_input, mtime_ns) == 16, "an input's mtime_ns is at 16");
    assert!(offset_of!(sharp_input, hash) == 24, "an input's hash is at 24");
    assert!(offset_of!(sharp_input, declarations_hash) == 40, "an input's declarations_hash is at 40");
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
    /// As `sharp_input` holds it: the `declarations_hash` of the compiled file of a `.sharp` file the compiled file
    /// reads, or all zero.
    pub declarations_hash: [u8; 16],
}

/// One declaration a check read, and the fingerprint of what it read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Read {
    pub name: Vec<u8>,
    pub fingerprint: u64,
}

/// What a file's check, and the extensions that analyzed it, read from other declarations. Only these enter the key,
/// so an edit that changes none of them leaves the compiled code's name unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Reads {
    /// Each declaration the check read through its signature, and each declaration an extension read in the analysis,
    /// for any file, with its ancestors, fingerprinted by codex's `DefSignatureNode::signature_hash`.
    pub signatures: Vec<Read>,
    /// Each method whose inferred return the check read, named `Class::method` and fingerprinted by the xxh3-64 of
    /// that return type's `TUnion::get_id()`.
    pub bodies: Vec<Read>,
    /// Each library method whose body the file inlines, named `Class::method` and fingerprinted by the xxh3-64 of the
    /// inlined form's bytes.
    pub inlined: Vec<Read>,
    /// Each set of names an extension listed in the analysis, for any file, since a hook may answer the file from a set
    /// it kept from another, fingerprinted by a hash of the names the set holds, so a name joining or leaving it changes
    /// the key. A method search is fingerprinted by the methods it finds with the fields it projects of them.
    pub listed: Vec<Read>,
    /// The function type of each method and lambda the file declares, as `Unit::function_types` gives them, since its
    /// effects come from the bodies it calls, in any file, fingerprinted by `function_type_fingerprint`.
    pub function_types: Vec<Read>,
}

/// The standard library a project's files compile against, as each compiled file's header names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    /// The native fingerprint its `native.php` gives. All zero when the project holds no standard library.
    pub native_fingerprint: [u8; 16],
    /// The version Composer installed it at. Empty when Composer reports none.
    pub version: Vec<u8>,
}

/// The fields of a header that passed load checks 1 to 3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub abi: [u8; 16],
    pub checker: [u8; 16],
    pub key: [u8; 16],
    pub source_hash: [u8; 16],
    pub source_size: u64,
    pub library: Library,
    pub declarations_hash: [u8; 16],
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
    /// The file's inputs, tree or texts differ from the ones its header's `sections_hash` names.
    Sections,
}

/// The xxh3-128 of a source, as the header's `source_hash` stores it.
#[must_use]
pub fn source_hash(source: &[u8]) -> [u8; 16] {
    xxh3_128(source).to_be_bytes()
}

/// The checker a compiled file of the standard library names: the PHP# release `version` that ships it.
///
/// It stands in place of a build ID, since PHP# compiles the library once, at its release, and every `mago` of that
/// release on every platform keeps it.
#[must_use]
pub fn release_checker(version: &str) -> [u8; 16] {
    xxh3_128(&[b"release ".as_slice(), version.as_bytes()].concat()).to_be_bytes()
}

/// The name of the code a check compiles.
///
/// It is the hash of the magic, `SHARP_UNIT_ABI`, `checker`, the build ID of the checker that compiles it, the source's
/// hash and the reads. The order the reads come in never changes it.
#[must_use]
pub fn key(checker: [u8; 16], source_hash: [u8; 16], reads: &Reads) -> [u8; 16] {
    key_of(&[&SHARP_UNIT_MAGIC, &SHARP_UNIT_ABI, &checker], source_hash, reads)
}

fn key_of(compiler: &[&[u8]], source_hash: [u8; 16], reads: &Reads) -> [u8; 16] {
    let mut hasher = Xxh3::new();
    for part in compiler {
        hasher.update(part);
    }
    hasher.update(&source_hash);

    for list in [&reads.signatures, &reads.bodies, &reads.inlined, &reads.listed, &reads.function_types] {
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

/// The hash of the declarations of a file whose signature is `signature`.
///
/// It covers each top-level declaration's name and `signature_hash`, with each member's, in the order the file
/// declares them. A body edit leaves it as it was, and an edit of a name, a parameter, a type, a modifier, an
/// attribute or a constant's value changes it.
#[must_use]
pub fn declarations_hash(signature: &FileSignature) -> [u8; 16] {
    let mut hasher = Xxh3::new();
    for node in &signature.ast_nodes {
        hasher.update(&size("members", node.children.len()).to_le_bytes());
        for declaration in std::iter::once(node).chain(&node.children) {
            let name = declaration.name.as_bytes();
            hasher.update(&size("a declaration's name", name.len()).to_le_bytes());
            hasher.update(name);
            hasher.update(&declaration.signature_hash.to_le_bytes());
        }
    }

    hasher.digest128().to_be_bytes()
}

/// The bytes of the `.sharpc` file for `unit`, lowered from `source` against `library` by the checker whose build ID is
/// `checker`.
///
/// Its header names `declarations_hash`, the hash of the source's declarations. The same arguments always give the
/// same bytes.
///
/// # Panics
///
/// Panics when a section holds more than a 32-bit size can say.
#[must_use]
pub fn encode(
    unit: &Unit,
    source: &[u8],
    key: [u8; 16],
    declarations_hash: [u8; 16],
    inputs: &[Input],
    checker: [u8; 16],
    library: &Library,
) -> Vec<u8> {
    let mut nodes =
        Vec::with_capacity(size_of::<sharp_node>() * unit.nodes.len() + size_of::<u32>() * unit.children.len());
    for node in &unit.nodes {
        nodes.extend_from_slice(&node_bytes(node));
    }
    for child in &unit.children {
        nodes.extend_from_slice(&child.to_le_bytes());
    }
    let code = Code {
        key,
        source_hash: source_hash(source),
        source_size: source.len() as u64,
        node_count: size("nodes", unit.nodes.len()),
        children_count: size("children", unit.children.len()),
        root: unit.root,
        tree: &nodes,
        texts: &unit.texts,
    };

    code.encode(declarations_hash, inputs, checker, library)
}

/// The bytes of the compiled file `bytes` with new declarations, inputs, checker and standard library.
///
/// `declarations_hash` is the hash of its source's declarations, `inputs` its inputs, `checker` the build ID of the
/// checker that writes it and `library` its standard library.
///
/// They are the bytes [`encode`] gives the unit `bytes` holds, with its key and its source. None when `bytes` is no
/// compiled file this bridge reads.
///
/// A compile keeps the code of each file whose key holds, and only what the file holds beside its code can differ from
/// the compile that wrote it.
///
/// # Panics
///
/// Panics when a section holds more than a 32-bit size can say.
#[must_use]
pub fn restamp(
    bytes: &[u8],
    declarations_hash: [u8; 16],
    inputs: &[Input],
    checker: [u8; 16],
    library: &Library,
) -> Option<Vec<u8>> {
    let header = header(bytes).ok()?;
    let word = |offset: usize| -> u32 {
        let mut word = [0; 4];
        word.copy_from_slice(&bytes[offset..offset + 4]);
        u32::from_le_bytes(word)
    };
    let (input_count, node_count, children_count) = (
        word(offset_of!(sharp_unit_header, input_count)) as usize,
        word(offset_of!(sharp_unit_header, node_count)),
        word(offset_of!(sharp_unit_header, children_count)),
    );
    let tree = size_of::<sharp_unit_header>() + size_of::<sharp_input>() * input_count;
    let texts = tree + size_of::<sharp_node>() * node_count as usize + size_of::<u32>() * children_count as usize;
    // `Code::encode` stores the library's version right after the unit's own texts, then each input's path.
    let own_texts = word(offset_of!(sharp_unit_header, library_version) + offset_of!(sharp_str, offset)) as usize;
    let code = Code {
        key: header.key,
        source_hash: header.source_hash,
        source_size: header.source_size,
        node_count,
        children_count,
        root: word(offset_of!(sharp_unit_header, root)),
        tree: &bytes[tree..texts],
        texts: &bytes[texts..texts + own_texts],
    };

    Some(code.encode(declarations_hash, inputs, checker, library))
}

/// The code a compiled file holds apart from its declarations' hash, its inputs and its standard library: its key, its
/// source's hash and size, and its tree, the nodes then the children as the file lays them out, with the texts they
/// point into.
struct Code<'code> {
    key: [u8; 16],
    source_hash: [u8; 16],
    source_size: u64,
    node_count: u32,
    children_count: u32,
    root: u32,
    tree: &'code [u8],
    texts: &'code [u8],
}

impl Code<'_> {
    /// The compiled file of this code, written by the checker build `checker`, with `declarations_hash`, `inputs` and
    /// `library`.
    fn encode(&self, declarations_hash: [u8; 16], inputs: &[Input], checker: [u8; 16], library: &Library) -> Vec<u8> {
        let mut texts = self.texts.to_vec();
        let library_version = store_text(&mut texts, &library.version);
        let inputs: Vec<sharp_input> = inputs
            .iter()
            .map(|input| sharp_input {
                path: store_text(&mut texts, &input.path),
                size: input.size,
                mtime_ns: input.mtime_ns,
                hash: input.hash,
                declarations_hash: input.declarations_hash,
            })
            .collect();

        let header = sharp_unit_header {
            magic: SHARP_UNIT_MAGIC,
            abi: SHARP_UNIT_ABI,
            checker,
            key: self.key,
            source_hash: self.source_hash,
            source_size: self.source_size,
            input_count: size("inputs", inputs.len()),
            node_count: self.node_count,
            children_count: self.children_count,
            root: self.root,
            texts_size: size("texts", texts.len()),
            native_fingerprint: library.native_fingerprint,
            library_version,
            sections_hash: [0; 16],
            declarations_hash,
        };

        let mut bytes = Vec::with_capacity(length(&header) as usize);
        bytes.extend_from_slice(&header_bytes(&header));
        for input in &inputs {
            bytes.extend_from_slice(&input_bytes(input));
        }
        bytes.extend_from_slice(self.tree);
        bytes.extend_from_slice(&texts);
        let sections_hash = sections_hash(&bytes);
        put(&mut bytes, offset_of!(sharp_unit_header, sections_hash), &sections_hash);

        bytes
    }
}

/// The xxh3-128 of every byte of the compiled file `bytes` after its header, as the header's `sections_hash` stores it.
fn sections_hash(bytes: &[u8]) -> [u8; 16] {
    xxh3_128(&bytes[size_of::<sharp_unit_header>()..]).to_be_bytes()
}

/// Reads the header of a `.sharpc` file, after load checks 1 to 3.
///
/// The magic and the ABI match this bridge's, the file's length is the length its header gives, and the sections
/// after the header hash to its `sections_hash`.
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
        native_fingerprint: hash(offset_of!(sharp_unit_header, native_fingerprint)),
        library_version: sharp_str {
            offset: word(offset_of!(sharp_unit_header, library_version) + offset_of!(sharp_str, offset)),
            len: word(offset_of!(sharp_unit_header, library_version) + offset_of!(sharp_str, len)),
        },
        sections_hash: hash(offset_of!(sharp_unit_header, sections_hash)),
        declarations_hash: hash(offset_of!(sharp_unit_header, declarations_hash)),
    };
    if header.abi != SHARP_UNIT_ABI {
        return Err(FormatError::Abi);
    }
    if bytes.len() as u64 != length(&header) {
        return Err(FormatError::Length);
    }
    if sections_hash(bytes) != header.sections_hash {
        return Err(FormatError::Sections);
    }

    let texts = size_of::<sharp_unit_header>()
        + size_of::<sharp_input>() * header.input_count as usize
        + size_of::<sharp_node>() * header.node_count as usize
        + size_of::<u32>() * header.children_count as usize;
    let version = header.library_version.offset as usize
        ..header.library_version.offset as usize + header.library_version.len as usize;
    let Some(version) = bytes[texts..texts + header.texts_size as usize].get(version) else {
        return Err(FormatError::Length);
    };

    Ok(Header {
        abi: header.abi,
        checker: header.checker,
        key: header.key,
        source_hash: header.source_hash,
        source_size: header.source_size,
        library: Library { native_fingerprint: header.native_fingerprint, version: version.to_vec() },
        declarations_hash: header.declarations_hash,
    })
}

/// The size, modification time and hash of the file at `path`, or none when no file of exactly that name is there.
///
/// A path is under `root`, or absolute for a file outside it, as one an includes folder that is a symlink reaches, and
/// the engine reads it the same way.
///
/// A path whose last segment is [`SOURCE_LISTING`] gets the stamp of its folder's listing, which a missing folder gives
/// empty.
///
/// # Errors
///
/// Returns the error of reading the file or the folder, other than its absence.
pub fn stamp(root: &Path, path: &[u8]) -> Result<Option<Input>, DatabaseError> {
    if let Some(folder) = path.strip_suffix(SOURCE_LISTING.as_bytes())
        && (folder.is_empty() || folder.ends_with(b"/"))
    {
        let listing = source_listing(&root.join(String::from_utf8_lossy(folder).as_ref()))?;

        return Ok(Some(Input {
            path: path.to_vec(),
            size: listing.len() as u64,
            mtime_ns: 0,
            hash: source_hash(&listing),
            declarations_hash: [0; 16],
        }));
    }

    let full = root.join(String::from_utf8_lossy(path).as_ref());
    // A path no file is at under its name in any case is absent, which a look at the path says without listing the
    // folder, as most of a compile's absent inputs are, a folder's `Module.sharp` among them.
    if matches!(full.try_exists(), Ok(false)) || !folder_holds_name(&full)? {
        return Ok(None);
    }
    // The modification time is taken before the contents. A write between the two leaves a stamp whose time is older
    // than the file's, so the engine hashes the file instead of trusting the time and size.
    let read = std::fs::metadata(&full)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| Ok((std::fs::read(&full)?, modified)));
    let (contents, modified) = match read {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &full, error)),
    };

    Ok(Some(file_stamp(path, &contents, nanos_since_epoch(modified))))
}

/// The stamp [`stamp`] gives `file` at its name, from the bytes the loader read.
///
/// It takes the modification time the loader took just before the read, so the compile hashes the bytes its analysis
/// read without reading the file again. None when `file` has no modification time, as a file no walk listed.
#[must_use]
pub fn loaded_stamp(file: &File) -> Option<Input> {
    file.modified_ns.map(|mtime_ns| file_stamp(&file.name, &file.contents, mtime_ns))
}

/// The stamp of the file at `path` that holds `contents` and was modified at `mtime_ns`.
fn file_stamp(path: &[u8], contents: &[u8], mtime_ns: i64) -> Input {
    Input {
        path: path.to_vec(),
        size: contents.len() as u64,
        mtime_ns,
        hash: source_hash(contents),
        declarations_hash: [0; 16],
    }
}

/// Whether the folder of the file at `full` holds a file of exactly its name, case included. A disk that ignores case,
/// as macOS's does by default, opens `environment.php` for `Environment.php`. Linux does not, so a file is there only
/// under its exact name, and a compile on either disk stamps the same inputs, as the engine's
/// `sharp_folder_holds_name` reads them. A folder that is not there holds nothing.
fn folder_holds_name(full: &Path) -> Result<bool, DatabaseError> {
    let folder = full.parent().unwrap_or(full);
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(DatabaseError::io(IOAction::OpenFolder, folder, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?;
        if Some(entry.file_name().as_os_str()) == full.file_name() {
            return Ok(true);
        }
    }

    Ok(false)
}

/// The listing of `folder` that [`SOURCE_LISTING`] stamps.
fn source_listing(folder: &Path) -> Result<Vec<u8>, DatabaseError> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DatabaseError::io(IOAction::OpenFolder, folder, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, folder, error))?;
        let mut name = entry.file_name().into_encoded_bytes();
        let source = name.len() > b".sharp".len() && name.ends_with(b".sharp");
        // A link whose target is gone, or a file deleted since the folder was read, is neither, as the loader and the
        // engine skip it.
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if source && metadata.is_file() {
            names.push(name);
        } else if metadata.is_dir() && name.first().is_some_and(u8::is_ascii_uppercase) {
            name.push(b'/');
            names.push(name);
        }
    }
    names.sort_unstable();

    Ok(names.into_iter().flat_map(|name| name.into_iter().chain(std::iter::once(b'\n'))).collect())
}

/// Where the compiled file of `source` lives, or none when `source` is a link to a file outside `root`.
///
/// The compiled file sits in the `.sharp` folder at the canonical `root`, at the real path of `source` relative to
/// `root`, with `.sharp` replaced by `.sharpc`. A source gone since the run read it, as one deleted or moved while the
/// run analyzed it, has no real path, so its compiled file sits at its own path, a stale input the next run picks up.
///
/// # Errors
///
/// Returns the error of resolving the real path of `source`, other than its absence.
pub fn compiled_path(root: &Path, source: &Path) -> Result<Option<PathBuf>, DatabaseError> {
    let real = match source.canonicalize() {
        Ok(real) => real,
        Err(error) if error.kind() == io::ErrorKind::NotFound => source.to_path_buf(),
        Err(error) => return Err(DatabaseError::io(IOAction::ResolvePath, source, error)),
    };

    Ok(compiled_path_of_real(root, &real))
}

/// Where the compiled file of the source whose real path is `real` lives, or none when `real` is outside `root`.
///
/// It is where [`compiled_path`] places it. A run that resolved each source's real path when it found the file, as the
/// walk of its load does, takes the compiled file's path from that real path.
#[must_use]
pub fn compiled_path_of_real(root: &Path, real: &Path) -> Option<PathBuf> {
    real.strip_prefix(root).ok().map(|relative| root.join(COMPILED_FOLDER).join(relative).with_extension("sharpc"))
}

/// Returns `true` when the compiled file `bytes` cannot stand for the code whose key is `key` any more.
///
/// It is no compiled file this bridge reads, a checker build other than `checker` wrote it, or it names another key.
///
/// The key hashes the source and the fingerprint of each declaration its check read, as [`key`] gives it, so a compile
/// that computes `key` from the fingerprints of the reads the file's last check named keeps the file while none of
/// them changed, whatever else an edit changed.
#[must_use]
pub fn is_stale(bytes: &[u8], checker: [u8; 16], key: [u8; 16]) -> bool {
    header(bytes).ok().is_none_or(|header| header.checker != checker || header.key != key)
}

/// Returns `true` when the compiled file `bytes` is no compiled file this bridge reads, or one of its inputs changed.
///
/// An input under `root` changed when it differs from its stamp, as the engine checks a compiled file's inputs before
/// it runs one, whichever checker build wrote it: when its size or hash changed, when it is gone, or when a file
/// appeared where it was absent. An input that names a `declarations_hash` stays current while its file is there and
/// the compiled file of that file, in the `.sharp` folder at `root`, names the same hash.
///
/// # Errors
///
/// Returns the error of reading an input or the compiled file of one.
pub fn inputs_changed(root: &Path, bytes: &[u8]) -> Result<bool, DatabaseError> {
    if header(bytes).is_err() {
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
        let declarations = &bytes[input + offset_of!(sharp_input, declarations_hash)..][..16];

        let stamped = stamp(root, path)?;
        let current = match &stamped {
            Some(stamped) => (stamped.size, stamped.hash),
            None => (0, [0; 16]),
        };
        if current == (u64::from_le_bytes(size), hash.try_into().unwrap_or([0; 16])) {
            continue;
        }
        if stamped.is_none() || declarations == [0; 16] || compiled_declarations(root, path)? != declarations {
            return Ok(true);
        }
    }

    Ok(false)
}

/// The `declarations_hash` the compiled file of the source `path`, under `root`, names, or all zero when it has no
/// compiled file this bridge reads, as the engine reads it from the header alone.
fn compiled_declarations(root: &Path, path: &[u8]) -> Result<[u8; 16], DatabaseError> {
    let Some(compiled) = compiled_path(root, &root.join(String::from_utf8_lossy(path).as_ref()))? else {
        return Ok([0; 16]);
    };
    let bytes = match std::fs::read(&compiled) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok([0; 16]),
        Err(error) => return Err(DatabaseError::io(IOAction::ReadFile, &compiled, error)),
    };

    Ok(header(&bytes).map_or([0; 16], |header| header.declarations_hash))
}

/// The length of the file `header` describes.
fn length(header: &sharp_unit_header) -> u64 {
    size_of::<sharp_unit_header>() as u64
        + size_of::<sharp_input>() as u64 * u64::from(header.input_count)
        + size_of::<sharp_node>() as u64 * u64::from(header.node_count)
        + size_of::<u32>() as u64 * u64::from(header.children_count)
        + u64::from(header.texts_size)
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
    put(&mut bytes, offset_of!(sharp_unit_header, native_fingerprint), &header.native_fingerprint);
    put_text(&mut bytes, offset_of!(sharp_unit_header, library_version), header.library_version);
    put(&mut bytes, offset_of!(sharp_unit_header, sections_hash), &header.sections_hash);
    put(&mut bytes, offset_of!(sharp_unit_header, declarations_hash), &header.declarations_hash);

    bytes
}

fn input_bytes(input: &sharp_input) -> [u8; size_of::<sharp_input>()] {
    let mut bytes = [0; size_of::<sharp_input>()];
    put_text(&mut bytes, offset_of!(sharp_input, path), input.path);
    put(&mut bytes, offset_of!(sharp_input, size), &input.size.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_input, mtime_ns), &input.mtime_ns.to_le_bytes());
    put(&mut bytes, offset_of!(sharp_input, hash), &input.hash);
    put(&mut bytes, offset_of!(sharp_input, declarations_hash), &input.declarations_hash);

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
