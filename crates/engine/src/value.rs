use std::borrow::Cow;
use std::ffi::c_char;

use crate::EngineError;

/// A PHP value as it crosses between Mago and the engine.
///
/// A list is a PHP array with the keys 0, 1, 2 and on in order, and a map is any other PHP array. A map's key that
/// reads as a decimal integer becomes an integer key, as it does in a PHP array literal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[allow(clippy::unsafe_derive_deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(Vec<u8>),
    List(Vec<Value>),
    Map(Vec<(Vec<u8>, Value)>),
    /// An object of the loaded class `class`, with no constructor run, and each instance property the class declares
    /// set once to its value, which its type takes as it is.
    Object {
        class: Cow<'static, str>,
        properties: Vec<(Cow<'static, str>, Value)>,
    },
    /// The case `case` of the loaded enum `enumeration`.
    Case {
        enumeration: Cow<'static, str>,
        case: Cow<'static, str>,
    },
}

/// The storage of one engine value, a `zval`, which the engine reads and writes through its own API.
#[repr(C, align(8))]
pub(crate) struct Zval([u8; 16]);

impl Zval {
    pub(crate) const fn new() -> Self {
        Self([0; 16])
    }
}

const IS_NULL: u8 = 1;
const IS_FALSE: u8 = 2;
const IS_TRUE: u8 = 3;
const IS_LONG: u8 = 4;
const IS_DOUBLE: u8 = 5;
const IS_STRING: u8 = 6;
const IS_ARRAY: u8 = 7;

unsafe extern "C" {
    fn engine_value_null(value: *mut Zval);
    fn engine_value_bool(value: *mut Zval, flag: bool);
    fn engine_value_int(value: *mut Zval, number: i64);
    fn engine_value_float(value: *mut Zval, number: f64);
    fn engine_value_string(value: *mut Zval, bytes: *const c_char, length: usize);
    fn engine_value_array(value: *mut Zval, size: u32);
    fn engine_array_push(array: *mut Zval) -> *mut Zval;
    fn engine_array_insert(array: *mut Zval, key: *const c_char, length: usize) -> *mut Zval;
    fn engine_value_object(value: *mut Zval, name: *const c_char, length: usize) -> bool;
    fn engine_object_set(object: *mut Zval, name: *const c_char, length: usize, property: *mut Zval) -> bool;
    fn engine_object_unset(object: *const Zval, length: *mut usize) -> *const c_char;
    fn engine_value_case(
        value: *mut Zval,
        enumeration: *const c_char,
        length: usize,
        name: *const c_char,
        name_length: usize,
    ) -> bool;
    pub(crate) fn engine_value_release(value: *mut Zval);
    fn engine_value_type(value: *const Zval) -> u8;
    fn engine_value_long(value: *const Zval) -> i64;
    fn engine_value_double(value: *const Zval) -> f64;
    fn engine_value_bytes(value: *const Zval, length: *mut usize) -> *const c_char;
    fn engine_array_is_list(value: *const Zval) -> bool;
    fn engine_array_count(value: *const Zval) -> u32;
    fn engine_array_entry(
        array: *const Zval,
        position: *mut u32,
        value: *mut *const Zval,
        key: *mut *const c_char,
        key_length: *mut usize,
        index: *mut i64,
    ) -> bool;
}

impl Value {
    /// Writes this value into `slot`, which the engine owns afterwards, also when an object in it can't be built.
    ///
    /// # Errors
    ///
    /// Returns an error when an object or a case in it names no loaded class, enum, property or case, or leaves a
    /// property unset or sets one to a value its type doesn't take.
    ///
    /// # Safety
    ///
    /// `slot` holds no value, and the calling thread runs an interpreter.
    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    pub(crate) unsafe fn write(&self, slot: *mut Zval) -> Result<(), EngineError> {
        // SAFETY: the caller gives an empty slot on a thread that runs an interpreter, and every pointer and length
        // passed here comes from a live Rust slice.
        unsafe {
            match self {
                Self::Null => engine_value_null(slot),
                Self::Bool(flag) => engine_value_bool(slot, *flag),
                Self::Int(number) => engine_value_int(slot, *number),
                Self::Float(number) => engine_value_float(slot, *number),
                Self::String(bytes) => engine_value_string(slot, bytes.as_ptr().cast(), bytes.len()),
                Self::List(items) => {
                    engine_value_array(slot, items.len() as u32);
                    for item in items {
                        item.write(engine_array_push(slot))?;
                    }
                }
                Self::Map(entries) => {
                    engine_value_array(slot, entries.len() as u32);
                    for (key, item) in entries {
                        item.write(engine_array_insert(slot, key.as_ptr().cast(), key.len()))?;
                    }
                }
                Self::Object { class, properties } => {
                    if !engine_value_object(slot, class.as_ptr().cast(), class.len()) {
                        engine_value_null(slot);
                        return Err(EngineError::Build(format!("no class `{class}` that can be built is loaded")));
                    }

                    for (name, property) in properties {
                        let mut value = Zval::new();
                        let written = property.write(&raw mut value);
                        if let Err(error) = written {
                            engine_value_release(&raw mut value);
                            return Err(error);
                        }

                        if !engine_object_set(slot, name.as_ptr().cast(), name.len(), &raw mut value) {
                            return Err(EngineError::Build(format!(
                                "`{class}` declares no unset instance property `{name}` whose type takes the value given"
                            )));
                        }
                    }

                    let mut length = 0;
                    let unset = engine_object_unset(slot, &raw mut length);
                    if !unset.is_null() {
                        let name = String::from_utf8_lossy(std::slice::from_raw_parts(unset.cast::<u8>(), length));
                        return Err(EngineError::Build(format!("`{class}` leaves its property `{name}` unset")));
                    }
                }
                Self::Case { enumeration, case } => {
                    if !engine_value_case(
                        slot,
                        enumeration.as_ptr().cast(),
                        enumeration.len(),
                        case.as_ptr().cast(),
                        case.len(),
                    ) {
                        engine_value_null(slot);
                        return Err(EngineError::Build(format!(
                            "no loaded enum `{enumeration}` has the case `{case}`"
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Reads the value `value` holds.
    ///
    /// # Errors
    ///
    /// Returns an error when it holds an object, a resource or anything else no [`Value`] holds.
    ///
    /// # Safety
    ///
    /// `value` points to a live engine value of the calling thread's interpreter.
    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    pub(crate) unsafe fn read(value: *const Zval) -> Result<Self, EngineError> {
        // SAFETY: the caller gives a live engine value of this thread's interpreter, and each read below matches the
        // type the engine reports for it.
        unsafe {
            Ok(match engine_value_type(value) {
                IS_NULL => Self::Null,
                IS_FALSE => Self::Bool(false),
                IS_TRUE => Self::Bool(true),
                IS_LONG => Self::Int(engine_value_long(value)),
                IS_DOUBLE => Self::Float(engine_value_double(value)),
                IS_STRING => {
                    let mut length = 0;
                    let bytes = engine_value_bytes(value, &raw mut length);
                    Self::String(std::slice::from_raw_parts(bytes.cast::<u8>(), length).to_vec())
                }
                IS_ARRAY => {
                    let list = engine_array_is_list(value);
                    let mut items = Vec::with_capacity(engine_array_count(value) as usize);
                    let mut entries = Vec::new();
                    let mut position = 0;
                    let mut item = std::ptr::null();
                    let mut key = std::ptr::null();
                    let mut key_length = 0;
                    let mut index = 0;
                    while engine_array_entry(
                        value,
                        &raw mut position,
                        &raw mut item,
                        &raw mut key,
                        &raw mut key_length,
                        &raw mut index,
                    ) {
                        let read = Self::read(item)?;
                        if list {
                            items.push(read);
                        } else if key.is_null() {
                            entries.push((index.to_string().into_bytes(), read));
                        } else {
                            entries.push((std::slice::from_raw_parts(key.cast::<u8>(), key_length).to_vec(), read));
                        }
                    }

                    if list { Self::List(items) } else { Self::Map(entries) }
                }
                kind => return Err(EngineError::Value(kind)),
            })
        }
    }
}
