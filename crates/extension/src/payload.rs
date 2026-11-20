//! Domain-neutral primitives for extension payloads.
//!
//! A message to an extension host is a list of engine values, one per field, which [`ValueWriter`] writes and
//! [`ValueReader`] reads in the order the field's codec gives them. A byte payload, which [`PayloadWriter`] and
//! [`PayloadReader`] write and read for a pipe to another process and for one field read whole, such as a table of
//! packed records or a method search, uses network byte order and prefixes each byte string with its length as a
//! `u32`. Capability-specific crates are responsible for defining message kinds, limits,
//! and validation.

#![allow(clippy::big_endian_bytes, reason = "network byte order is part of the stable extension wire format")]

use mago_engine::Value;

use crate::error::PayloadError;

/// A writer for stable binary extension payloads.
#[derive(Debug, Default)]
pub struct PayloadWriter {
    bytes: Vec<u8>,
}

impl PayloadWriter {
    /// Creates an empty payload writer.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Creates an empty payload writer with space for at least `capacity` bytes.
    #[must_use]
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { bytes: Vec::with_capacity(capacity) }
    }

    /// Appends bytes without a length prefix.
    #[inline]
    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    #[inline]
    pub fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    #[inline]
    pub fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    #[inline]
    pub fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    #[inline]
    pub fn write_u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    #[inline]
    pub fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    /// Writes a collection length as a `u32`.
    ///
    /// # Errors
    ///
    /// Returns an error when `length` does not fit in a `u32`.
    #[inline]
    pub fn write_length(&mut self, length: usize) -> Result<(), PayloadError> {
        let length = u32::try_from(length).map_err(|_| PayloadError::LengthOverflow { length })?;
        self.write_u32(length);
        Ok(())
    }

    /// Writes length-prefixed bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the byte length does not fit in a `u32`.
    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), PayloadError> {
        self.write_length(bytes.len())?;
        self.write_raw(bytes);
        Ok(())
    }

    /// Writes a length-prefixed UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoded byte length does not fit in a `u32`.
    #[inline]
    pub fn write_string(&mut self, value: &str) -> Result<(), PayloadError> {
        self.write_bytes(value.as_bytes())
    }

    /// Returns the current payload length.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the payload is empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the completed payload.
    #[must_use]
    #[inline]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// A zero-copy reader for stable binary extension payloads.
#[derive(Debug)]
pub struct PayloadReader<'payload> {
    payload: &'payload [u8],
    offset: usize,
}

impl<'payload> PayloadReader<'payload> {
    #[must_use]
    #[inline]
    pub const fn new(payload: &'payload [u8]) -> Self {
        Self { payload, offset: 0 }
    }

    /// Reads exactly `length` bytes without a length prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested range overflows or the payload ends.
    #[inline]
    pub fn read_raw(&mut self, length: usize, field: &'static str) -> Result<&'payload [u8], PayloadError> {
        let end = self.offset.checked_add(length).ok_or(PayloadError::OffsetOverflow { field })?;
        let bytes = self.payload.get(self.offset..end).ok_or(PayloadError::UnexpectedEnd {
            field,
            needed: length,
            remaining: self.remaining(),
        })?;
        self.offset = end;
        Ok(bytes)
    }

    /// Reads a fixed-size byte array without a length prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload does not contain `N` more bytes.
    #[inline]
    pub fn read_array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], PayloadError> {
        let mut value = [0; N];
        value.copy_from_slice(self.read_raw(N, field)?);
        Ok(value)
    }

    /// Reads an unsigned byte.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload has no byte remaining.
    #[inline]
    pub fn read_u8(&mut self, field: &'static str) -> Result<u8, PayloadError> {
        Ok(self.read_array::<1>(field)?[0])
    }

    /// Reads a big-endian `u16`.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload has fewer than two bytes remaining.
    #[inline]
    pub fn read_u16(&mut self, field: &'static str) -> Result<u16, PayloadError> {
        Ok(u16::from_be_bytes(self.read_array(field)?))
    }

    /// Reads a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload has fewer than four bytes remaining.
    #[inline]
    pub fn read_u32(&mut self, field: &'static str) -> Result<u32, PayloadError> {
        Ok(u32::from_be_bytes(self.read_array(field)?))
    }

    /// Reads a big-endian `u64`.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload has fewer than eight bytes remaining.
    #[inline]
    pub fn read_u64(&mut self, field: &'static str) -> Result<u64, PayloadError> {
        Ok(u64::from_be_bytes(self.read_array(field)?))
    }

    /// Reads a boolean encoded as zero or one.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload ends or contains another value.
    #[inline]
    pub fn read_bool(&mut self, field: &'static str) -> Result<bool, PayloadError> {
        match self.read_u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(PayloadError::InvalidBoolean { field, value }),
        }
    }

    /// Reads a collection length and validates it against `maximum`.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload ends or the decoded count is too large.
    #[inline]
    pub fn read_count(&mut self, field: &'static str, maximum: usize) -> Result<usize, PayloadError> {
        let count = self.read_u32(field)? as usize;
        if count > maximum {
            return Err(PayloadError::CountExceeded { field, count, maximum });
        }

        Ok(count)
    }

    /// Reads length-prefixed bytes without copying them.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload ends before the byte string does.
    #[inline]
    pub fn read_bytes(&mut self, field: &'static str) -> Result<&'payload [u8], PayloadError> {
        let length = self.read_u32(field)? as usize;
        self.read_raw(length, field)
    }

    /// Reads a length-prefixed UTF-8 string without copying it.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload ends or the bytes are not valid UTF-8.
    #[inline]
    pub fn read_str(&mut self, field: &'static str) -> Result<&'payload str, PayloadError> {
        std::str::from_utf8(self.read_bytes(field)?).map_err(|_| PayloadError::InvalidUtf8 { field })
    }

    /// Reads an owned length-prefixed UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload ends or the bytes are not valid UTF-8.
    #[inline]
    pub fn read_string(&mut self, field: &'static str) -> Result<String, PayloadError> {
        self.read_str(field).map(str::to_owned)
    }

    /// Validates that the entire payload was consumed.
    ///
    /// # Errors
    ///
    /// Returns an error when unread bytes remain.
    #[inline]
    pub fn finish(self) -> Result<(), PayloadError> {
        let remaining = self.remaining();
        if remaining == 0 { Ok(()) } else { Err(PayloadError::TrailingBytes { remaining }) }
    }

    #[must_use]
    #[inline]
    pub const fn remaining(&self) -> usize {
        self.payload.len() - self.offset
    }
}

/// A writer for the fields of an extension host message, each one engine value.
#[derive(Debug, Default)]
pub struct ValueWriter {
    values: Vec<Value>,
}

impl ValueWriter {
    /// Creates an empty writer.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Creates an empty writer with space for at least `capacity` fields.
    #[must_use]
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { values: Vec::with_capacity(capacity) }
    }

    #[inline]
    pub fn write_u8(&mut self, value: u8) {
        self.values.push(Value::Int(i64::from(value)));
    }

    #[inline]
    pub fn write_u16(&mut self, value: u16) {
        self.values.push(Value::Int(i64::from(value)));
    }

    #[inline]
    pub fn write_u32(&mut self, value: u32) {
        self.values.push(Value::Int(i64::from(value)));
    }

    /// Writes `value` as the PHP integer with its bits, which is negative past `i64::MAX`.
    #[inline]
    pub fn write_u64(&mut self, value: u64) {
        self.values.push(Value::Int(value.cast_signed()));
    }

    #[inline]
    pub fn write_f64(&mut self, value: f64) {
        self.values.push(Value::Float(value));
    }

    #[inline]
    pub fn write_bool(&mut self, value: bool) {
        self.values.push(Value::Bool(value));
    }

    /// Writes a collection length.
    ///
    /// # Errors
    ///
    /// Returns an error when `length` does not fit in a `u32`, the most any count of the protocol holds.
    #[inline]
    pub fn write_length(&mut self, length: usize) -> Result<(), PayloadError> {
        let length = u32::try_from(length).map_err(|_| PayloadError::LengthOverflow { length })?;
        self.write_u32(length);
        Ok(())
    }

    /// Writes a byte string.
    ///
    /// # Errors
    ///
    /// Returns an error when its length does not fit in a `u32`, the longest string of the protocol.
    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), PayloadError> {
        u32::try_from(bytes.len()).map_err(|_| PayloadError::LengthOverflow { length: bytes.len() })?;
        self.values.push(Value::String(bytes.to_vec()));
        Ok(())
    }

    /// Writes a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error when its length does not fit in a `u32`.
    #[inline]
    pub fn write_string(&mut self, value: &str) -> Result<(), PayloadError> {
        self.write_bytes(value.as_bytes())
    }

    /// Writes an optional UTF-8 string, as null when there is none.
    ///
    /// # Errors
    ///
    /// Returns an error when its length does not fit in a `u32`.
    #[inline]
    pub fn write_optional_string(&mut self, value: Option<&str>) -> Result<(), PayloadError> {
        match value {
            Some(value) => self.write_string(value),
            None => {
                self.values.push(Value::Null);
                Ok(())
            }
        }
    }

    /// Writes one value as one field, such as the list another writer finished.
    #[inline]
    pub fn write_value(&mut self, value: Value) {
        self.values.push(value);
    }

    /// Writes the fields another writer finished, in order, as fields of this message.
    #[inline]
    pub fn write_values(&mut self, values: &[Value]) {
        self.values.extend_from_slice(values);
    }

    /// The number of fields written.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the fields written, in order.
    #[must_use]
    #[inline]
    pub fn finish(self) -> Vec<Value> {
        self.values
    }
}

/// A reader for the fields of an extension host message, each one engine value, which borrows the strings it reads.
#[derive(Debug, Clone)]
pub struct ValueReader<'values> {
    values: &'values [Value],
    position: usize,
}

impl<'values> ValueReader<'values> {
    #[must_use]
    #[inline]
    pub const fn new(values: &'values [Value]) -> Self {
        Self { values, position: 0 }
    }

    /// Reads the next field as it is.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read.
    #[inline]
    pub fn read_value(&mut self, field: &'static str) -> Result<&'values Value, PayloadError> {
        let value = self.values.get(self.position).ok_or(PayloadError::MissingValue { field })?;
        self.position += 1;
        Ok(value)
    }

    /// Reads the next field, a list, as a reader of its own.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read or the field is no list.
    #[inline]
    pub fn read_list(&mut self, field: &'static str) -> Result<ValueReader<'values>, PayloadError> {
        match self.read_value(field)? {
            Value::List(values) => Ok(ValueReader::new(values)),
            _ => Err(PayloadError::UnexpectedValue { field, expected: "a list" }),
        }
    }

    #[inline]
    fn read_int(&mut self, field: &'static str) -> Result<i64, PayloadError> {
        match self.read_value(field)? {
            Value::Int(value) => Ok(*value),
            _ => Err(PayloadError::UnexpectedValue { field, expected: "an integer" }),
        }
    }

    #[inline]
    fn read_unsigned<T>(&mut self, field: &'static str) -> Result<T, PayloadError>
    where
        T: TryFrom<i64>,
    {
        let value = self.read_int(field)?;
        T::try_from(value).map_err(|_| PayloadError::OutOfRange { field, value })
    }

    /// Reads an integer from 0 to 255.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is no integer in that range.
    #[inline]
    pub fn read_u8(&mut self, field: &'static str) -> Result<u8, PayloadError> {
        self.read_unsigned(field)
    }

    /// Reads an integer from 0 to 65,535.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is no integer in that range.
    #[inline]
    pub fn read_u16(&mut self, field: &'static str) -> Result<u16, PayloadError> {
        self.read_unsigned(field)
    }

    /// Reads an integer from 0 to `u32::MAX`.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is no integer in that range.
    #[inline]
    pub fn read_u32(&mut self, field: &'static str) -> Result<u32, PayloadError> {
        self.read_unsigned(field)
    }

    /// Reads a PHP integer's bits as a `u64`, as [`ValueWriter::write_u64`] writes one.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read or the field is no integer.
    #[inline]
    pub fn read_u64(&mut self, field: &'static str) -> Result<u64, PayloadError> {
        self.read_int(field).map(i64::cast_unsigned)
    }

    /// Reads a float.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read or the field is no float.
    #[inline]
    pub fn read_f64(&mut self, field: &'static str) -> Result<f64, PayloadError> {
        match self.read_value(field)? {
            Value::Float(value) => Ok(*value),
            _ => Err(PayloadError::UnexpectedValue { field, expected: "a float" }),
        }
    }

    /// Reads a boolean.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read or the field is no boolean.
    #[inline]
    pub fn read_bool(&mut self, field: &'static str) -> Result<bool, PayloadError> {
        match self.read_value(field)? {
            Value::Bool(value) => Ok(*value),
            _ => Err(PayloadError::UnexpectedValue { field, expected: "a boolean" }),
        }
    }

    /// Reads a collection length and validates it against `maximum`.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, the field is no integer from 0 to `u32::MAX`, or it is past
    /// `maximum`.
    #[inline]
    pub fn read_count(&mut self, field: &'static str, maximum: usize) -> Result<usize, PayloadError> {
        let count = self.read_u32(field)? as usize;
        if count > maximum {
            return Err(PayloadError::CountExceeded { field, count, maximum });
        }

        Ok(count)
    }

    /// Reads a byte string without copying it.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read or the field is no string.
    #[inline]
    pub fn read_bytes(&mut self, field: &'static str) -> Result<&'values [u8], PayloadError> {
        match self.read_value(field)? {
            Value::String(bytes) => Ok(bytes),
            _ => Err(PayloadError::UnexpectedValue { field, expected: "a string" }),
        }
    }

    /// Reads a UTF-8 string without copying it.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is no string or not valid UTF-8.
    #[inline]
    pub fn read_str(&mut self, field: &'static str) -> Result<&'values str, PayloadError> {
        std::str::from_utf8(self.read_bytes(field)?).map_err(|_| PayloadError::InvalidUtf8 { field })
    }

    /// Reads an owned UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is no string or not valid UTF-8.
    #[inline]
    pub fn read_string(&mut self, field: &'static str) -> Result<String, PayloadError> {
        self.read_str(field).map(str::to_owned)
    }

    /// Reads optional bytes, which null leaves out.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is neither null nor a string.
    #[inline]
    pub fn read_optional_bytes(&mut self, field: &'static str) -> Result<Option<&'values [u8]>, PayloadError> {
        if matches!(self.values.get(self.position), Some(Value::Null)) {
            self.position += 1;
            return Ok(None);
        }

        self.read_bytes(field).map(Some)
    }

    /// Reads an optional owned UTF-8 string, which null leaves out.
    ///
    /// # Errors
    ///
    /// Returns an error when every field was read, or the field is neither null nor a valid UTF-8 string.
    #[inline]
    pub fn read_optional_string(&mut self, field: &'static str) -> Result<Option<String>, PayloadError> {
        self.read_optional_bytes(field)?
            .map(|bytes| std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| PayloadError::InvalidUtf8 { field }))
            .transpose()
    }

    /// Validates that every field was read.
    ///
    /// # Errors
    ///
    /// Returns an error when unread fields remain.
    #[inline]
    pub fn finish(self) -> Result<(), PayloadError> {
        let remaining = self.remaining();
        if remaining == 0 { Ok(()) } else { Err(PayloadError::TrailingValues { remaining }) }
    }

    /// The number of fields not read yet.
    #[must_use]
    #[inline]
    pub const fn remaining(&self) -> usize {
        self.values.len() - self.position
    }

    /// The fields not read yet.
    #[must_use]
    #[inline]
    pub fn rest(&self) -> &'values [Value] {
        &self.values[self.position..]
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_payload_primitives() {
        let mut writer = PayloadWriter::new();
        writer.write_u8(1);
        writer.write_u16(0x0203);
        writer.write_u32(0x0405_0607);
        writer.write_u64(0x0809_0a0b_0c0d_0e0f);
        writer.write_bool(false);
        writer.write_bytes(b"bytes").unwrap();
        writer.write_string("string").unwrap();

        let payload = writer.finish();
        let mut reader = PayloadReader::new(&payload);
        assert_eq!(reader.read_u8("u8").unwrap(), 1);
        assert_eq!(reader.read_u16("u16").unwrap(), 0x0203);
        assert_eq!(reader.read_u32("u32").unwrap(), 0x0405_0607);
        assert_eq!(reader.read_u64("u64").unwrap(), 0x0809_0a0b_0c0d_0e0f);
        assert!(!reader.read_bool("bool").unwrap());
        assert_eq!(reader.read_bytes("bytes").unwrap(), b"bytes");
        assert_eq!(reader.read_str("string").unwrap(), "string");
        reader.finish().unwrap();
    }

    #[test]
    fn rejects_invalid_and_truncated_payloads() {
        let mut reader = PayloadReader::new(&[2]);
        assert!(matches!(reader.read_bool("flag"), Err(PayloadError::InvalidBoolean { field: "flag", value: 2 })));

        let mut reader = PayloadReader::new(&[0, 0, 0, 2, 0xff, 0xff]);
        assert!(matches!(reader.read_str("text"), Err(PayloadError::InvalidUtf8 { field: "text" })));

        let mut reader = PayloadReader::new(&[0, 0, 0, 2, 1]);
        assert!(matches!(reader.read_bytes("bytes"), Err(PayloadError::UnexpectedEnd { field: "bytes", .. })));
    }

    #[test]
    fn enforces_counts_and_complete_consumption() {
        let mut reader = PayloadReader::new(&[0, 0, 0, 2]);
        assert!(matches!(
            reader.read_count("items", 1),
            Err(PayloadError::CountExceeded { field: "items", count: 2, maximum: 1 })
        ));

        let reader = PayloadReader::new(&[1]);
        assert!(matches!(reader.finish(), Err(PayloadError::TrailingBytes { remaining: 1 })));
    }

    #[test]
    fn a_value_message_reads_back_each_field_as_written() {
        let mut nested = ValueWriter::new();
        nested.write_u8(9);
        let mut writer = ValueWriter::new();
        writer.write_u8(1);
        writer.write_u16(0x0203);
        writer.write_u32(0x0405_0607);
        writer.write_u64(u64::MAX);
        writer.write_f64(3.5);
        writer.write_bool(true);
        writer.write_bytes(b"bytes").unwrap();
        writer.write_string("string").unwrap();
        writer.write_optional_string(Some("optional")).unwrap();
        writer.write_optional_string(None).unwrap();
        writer.write_value(Value::List(nested.finish()));

        let values = writer.finish();
        assert_eq!(values[3], Value::Int(-1));
        let mut reader = ValueReader::new(&values);
        assert_eq!(reader.read_u8("u8").unwrap(), 1);
        assert_eq!(reader.read_u16("u16").unwrap(), 0x0203);
        assert_eq!(reader.read_u32("u32").unwrap(), 0x0405_0607);
        assert_eq!(reader.read_u64("u64").unwrap(), u64::MAX);
        assert_eq!(reader.read_f64("f64").unwrap(), 3.5);
        assert!(reader.read_bool("bool").unwrap());
        assert_eq!(reader.read_bytes("bytes").unwrap(), b"bytes");
        assert_eq!(reader.read_str("string").unwrap(), "string");
        assert_eq!(reader.read_optional_string("optional").unwrap().as_deref(), Some("optional"));
        assert_eq!(reader.read_optional_string("optional").unwrap(), None);
        let mut list = reader.read_list("nested").unwrap();
        assert_eq!(list.read_u8("nested u8").unwrap(), 9);
        list.finish().unwrap();
        reader.finish().unwrap();
    }

    #[test]
    fn a_value_reader_refuses_a_field_of_another_type_or_range_and_one_left_unread() {
        let values = [Value::Int(256), Value::Int(1), Value::String(vec![0xff]), Value::Null];
        let mut reader = ValueReader::new(&values);
        assert_eq!(reader.read_u8("small"), Err(PayloadError::OutOfRange { field: "small", value: 256 }));
        assert_eq!(
            reader.read_bool("flag"),
            Err(PayloadError::UnexpectedValue { field: "flag", expected: "a boolean" })
        );
        assert_eq!(reader.read_str("text"), Err(PayloadError::InvalidUtf8 { field: "text" }));
        assert_eq!(reader.clone().finish(), Err(PayloadError::TrailingValues { remaining: 1 }));
        assert_eq!(reader.read_value("null"), Ok(&Value::Null));
        assert_eq!(reader.read_u32("past the end"), Err(PayloadError::MissingValue { field: "past the end" }));
    }
}
