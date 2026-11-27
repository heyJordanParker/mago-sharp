//! The return-type requests an analysis with no extension host would have sent one, each kept as bytes in the file
//! that makes it, so the hosts answer them once they start without the file being analyzed again.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

use foldhash::HashMap;

use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_extension::Value;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::external::CallableTarget;
use crate::external::ExternalAnalyzerHandle;
use crate::external::error::ExternalAnalyzerError;
use crate::external::error::protocol;
use crate::external::protocol::ProviderTarget;
use crate::external::protocol::encode_return_type_invocation;
use crate::invocation::Invocation;

/// The return-type requests of each file an analysis with no extension host recorded, in the order its analysis made
/// them, as [`crate::plugin::PluginRegistry::take_return_type_requests`] gives them.
#[derive(Debug, Default)]
pub struct ReturnTypeRequests {
    files: HashMap<FileId, Box<[u8]>>,
    names: Arc<[Cow<'static, str>]>,
}

/// One recorded request: its target, and the invocation it sends, or none when the analysis could not make it, which
/// an analysis of the file with the hosts fails to make too.
pub(super) struct ReturnTypeRequest {
    pub class: Option<Vec<u8>>,
    pub name: Vec<u8>,
    pub invocation: Option<Value>,
}

impl ReturnTypeRequest {
    pub(super) fn target(&self) -> CallableTarget<'_> {
        match &self.class {
            Some(class) => CallableTarget::Method { class, method: &self.name },
            None => CallableTarget::Function(&self.name),
        }
    }
}

impl ReturnTypeRequests {
    /// Each file whose analysis made a recorded request.
    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.keys().copied()
    }

    /// The requests of `file_id`, in the order its analysis made them.
    pub(super) fn requests_of(
        &self,
        file_id: FileId,
    ) -> impl Iterator<Item = Result<ReturnTypeRequest, ExternalAnalyzerError>> + '_ {
        let mut bytes = self.files.get(&file_id).map_or(&[][..], |bytes| &**bytes);
        std::iter::from_fn(move || (!bytes.is_empty()).then(|| self.request(&mut bytes)))
    }

    fn request(&self, bytes: &mut &[u8]) -> Result<ReturnTypeRequest, ExternalAnalyzerError> {
        let kind = take_byte(bytes)?;
        let class = if kind & METHOD == 0 { None } else { Some(take_bytes(bytes)?.to_vec()) };
        let name = take_bytes(bytes)?.to_vec();
        let invocation = if kind & UNMADE == 0 { Some(self.value(bytes)?) } else { None };

        Ok(ReturnTypeRequest { class, name, invocation })
    }

    fn value(&self, bytes: &mut &[u8]) -> Result<Value, ExternalAnalyzerError> {
        Ok(match take_byte(bytes)? {
            NULL => Value::Null,
            FALSE => Value::Bool(false),
            TRUE => Value::Bool(true),
            INT => {
                let zigzag = take_number(bytes)?;
                Value::Int(((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64))
            }
            FLOAT => Value::Float(f64::from_bits(take_number(bytes)?)),
            STRING => Value::String(take_bytes(bytes)?.to_vec()),
            LIST => {
                let count = take_count(bytes)?;
                Value::List(std::iter::repeat_with(|| self.value(bytes)).take(count).collect::<Result<_, _>>()?)
            }
            MAP => {
                let count = take_count(bytes)?;
                let entries = std::iter::repeat_with(|| Ok((take_bytes(bytes)?.to_vec(), self.value(bytes)?)));
                Value::Map(entries.take(count).collect::<Result<_, ExternalAnalyzerError>>()?)
            }
            OBJECT => {
                let class = self.name(bytes)?;
                let count = take_count(bytes)?;
                let properties = std::iter::repeat_with(|| Ok((self.name(bytes)?, self.value(bytes)?)));
                let properties = properties.take(count).collect::<Result<_, ExternalAnalyzerError>>()?;
                Value::Object { class, properties }
            }
            CASE => Value::Case { enumeration: self.name(bytes)?, case: self.name(bytes)? },
            tag => return Err(protocol(format!("a recorded value has the unknown tag {tag}"))),
        })
    }

    fn name(&self, bytes: &mut &[u8]) -> Result<Cow<'static, str>, ExternalAnalyzerError> {
        let index = take_number(bytes)?;
        usize::try_from(index)
            .ok()
            .and_then(|index| self.names.get(index).cloned())
            .ok_or_else(|| protocol(format!("a recorded value names the unknown name {index}")))
    }
}

/// The record a registry keeps while the analysis that records its return-type requests runs: the requests the
/// analysis with `hosts` would send them.
#[derive(Debug)]
pub(crate) struct ReturnTypeRecord {
    hosts: Arc<ExternalAnalyzerHandle>,
    files: Mutex<Option<HashMap<FileId, Vec<u8>>>>,
}

impl ReturnTypeRecord {
    pub(crate) fn recording(hosts: Arc<ExternalAnalyzerHandle>) -> Self {
        Self { hosts, files: Mutex::new(Some(HashMap::default())) }
    }

    /// Records the return-type request of a call to `name`, a method of `class` when it is given, which `source_file`'s
    /// analysis makes once the native providers declined it, when a provider of the hosts targets the call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        class: Option<&[u8]>,
        name: &[u8],
        invocation: &Invocation<'_, '_, '_>,
        artifacts: &AnalysisArtifacts,
        source_file: &File,
        codebase: &CodebaseMetadata,
        calling_function_like: Option<FunctionLikeIdentifier>,
    ) {
        let called = match class {
            Some(class) => CallableTarget::Method { class, method: name },
            None => CallableTarget::Function(name),
        };
        if self.hosts.read(|hosts| hosts.provides_return_type(called, codebase)).is_ok_and(|provided| !provided) {
            return;
        }
        let target = match class {
            Some(class) => ProviderTarget::Method {
                class: codebase.get_class_like(word(class)).map_or(class, |metadata| metadata.original_name.as_bytes()),
                method: name,
            },
            None => ProviderTarget::Function(name),
        };
        let encoded =
            encode_return_type_invocation(target, invocation, artifacts, source_file, codebase, calling_function_like);
        let mut bytes = Vec::new();
        bytes.push(if class.is_some() { METHOD } else { 0 } | if encoded.is_err() { UNMADE } else { 0 });
        if let Some(class) = class {
            put_bytes(class, &mut bytes);
        }
        put_bytes(name, &mut bytes);
        if let Ok(invocation) = &encoded {
            put_value(invocation, &mut bytes);
        }

        if let Some(files) = self.files.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            files.entry(source_file.id).or_default().extend_from_slice(&bytes);
        }
    }

    /// The requests recorded so far, after which the record records none.
    pub(crate) fn take(&self) -> ReturnTypeRequests {
        let files = self.files.lock().unwrap_or_else(PoisonError::into_inner).take().unwrap_or_default();

        ReturnTypeRequests {
            files: files.into_iter().map(|(file_id, bytes)| (file_id, bytes.into_boxed_slice())).collect(),
            names: NAMES.lock().unwrap_or_else(PoisonError::into_inner).as_slice().into(),
        }
    }
}

/// The flag of a request's first byte that marks a method's request, which names its class next.
const METHOD: u8 = 1;
/// The flag of a request's first byte that marks a request the analysis could not make, which holds no invocation.
const UNMADE: u8 = 2;

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT: u8 = 3;
const FLOAT: u8 = 4;
const STRING: u8 = 5;
const LIST: u8 = 6;
const MAP: u8 = 7;
const OBJECT: u8 = 8;
const CASE: u8 = 9;

/// The protocol's class, property, enum and case names a recorded value names, by the index the value holds. The
/// protocol writes each as a literal, so the table holds each of a few hundred names once.
static NAMES: Mutex<Vec<Cow<'static, str>>> = Mutex::new(Vec::new());

thread_local! {
    /// The index in [`NAMES`] of each name this thread recorded, by the name.
    static NAME_INDEX: RefCell<HashMap<Cow<'static, str>, u64>> = RefCell::default();
}

fn put_name(name: Cow<'static, str>, bytes: &mut Vec<u8>) {
    let index = NAME_INDEX.with(|index| {
        let mut index = index.borrow_mut();
        if let Some(found) = index.get(name.as_ref()) {
            return *found;
        }
        let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
        names.push(name.clone());
        let found = (names.len() - 1) as u64;
        index.insert(name, found);

        found
    });
    put_number(index, bytes);
}

fn put_number(mut number: u64, bytes: &mut Vec<u8>) {
    while number >= 0x80 {
        bytes.push((number as u8) | 0x80);
        number >>= 7;
    }
    bytes.push(number as u8);
}

fn put_bytes(string: &[u8], bytes: &mut Vec<u8>) {
    put_number(string.len() as u64, bytes);
    bytes.extend_from_slice(string);
}

fn put_value(value: &Value, bytes: &mut Vec<u8>) {
    match value {
        Value::Null => bytes.push(NULL),
        Value::Bool(false) => bytes.push(FALSE),
        Value::Bool(true) => bytes.push(TRUE),
        Value::Int(number) => {
            bytes.push(INT);
            put_number(((number << 1) ^ (number >> 63)) as u64, bytes);
        }
        Value::Float(number) => {
            bytes.push(FLOAT);
            put_number(number.to_bits(), bytes);
        }
        Value::String(string) => {
            bytes.push(STRING);
            put_bytes(string, bytes);
        }
        Value::List(items) => {
            bytes.push(LIST);
            put_number(items.len() as u64, bytes);
            for item in items {
                put_value(item, bytes);
            }
        }
        Value::Map(entries) => {
            bytes.push(MAP);
            put_number(entries.len() as u64, bytes);
            for (key, item) in entries {
                put_bytes(key, bytes);
                put_value(item, bytes);
            }
        }
        Value::Object { class, properties } => {
            bytes.push(OBJECT);
            put_name(class.clone(), bytes);
            put_number(properties.len() as u64, bytes);
            for (property, item) in properties {
                put_name(property.clone(), bytes);
                put_value(item, bytes);
            }
        }
        Value::Case { enumeration, case } => {
            bytes.push(CASE);
            put_name(enumeration.clone(), bytes);
            put_name(case.clone(), bytes);
        }
    }
}

fn take_byte(bytes: &mut &[u8]) -> Result<u8, ExternalAnalyzerError> {
    let (byte, rest) = bytes.split_first().ok_or_else(|| protocol("a recorded request is cut short"))?;
    *bytes = rest;
    Ok(*byte)
}

fn take_number(bytes: &mut &[u8]) -> Result<u64, ExternalAnalyzerError> {
    let mut number = 0;
    for shift in (0..64).step_by(7) {
        let byte = take_byte(bytes)?;
        number |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(number);
        }
    }

    Err(protocol("a recorded number runs past 64 bits"))
}

fn take_count(bytes: &mut &[u8]) -> Result<usize, ExternalAnalyzerError> {
    usize::try_from(take_number(bytes)?).map_err(|_| protocol("a recorded count runs past the address space"))
}

fn take_bytes<'bytes>(bytes: &mut &'bytes [u8]) -> Result<&'bytes [u8], ExternalAnalyzerError> {
    let length = take_count(bytes)?;
    let (string, rest) = bytes.split_at_checked(length).ok_or_else(|| protocol("a recorded string is cut short"))?;
    *bytes = rest;
    Ok(string)
}
