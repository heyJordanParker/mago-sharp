//! The worker management messages a host's worker answers beside its extensions' requests: terminal worker-state reduction, and the
//! question whether a worker loaded one of some classes.

use mago_engine::Value;

use crate::ValueReader;
use crate::ValueWriter;
use crate::WorkerError;

/// The name every worker management message starts with, which the SDK routes it by.
const PROTOCOL: &[u8] = b"worker";
const COLLECT_REQUEST: u16 = 1;
const REDUCE_REQUEST: u16 = 2;
const CLASS_LOADS_REQUEST: u16 = 3;
const COLLECT_RESPONSE: u16 = 0x8001;
const REDUCE_RESPONSE: u16 = 0x8002;
const CLASS_LOADS_RESPONSE: u16 = 0x8003;
const MAXIMUM_REDUCERS: usize = 0x0000_4000;

pub(crate) fn collect_request() -> Vec<Value> {
    message_writer(COLLECT_REQUEST).finish()
}

/// Asks a worker whether its class loads hold one of `classes`, each a lowercase class name.
pub(crate) fn class_loads_request(classes: &[&[u8]]) -> Result<Vec<Value>, WorkerError> {
    let mut writer = message_writer(CLASS_LOADS_REQUEST);
    writer.write_length(classes.len()).map_err(invalid_response)?;
    for class in classes {
        writer.write_bytes(class).map_err(invalid_response)?;
    }

    Ok(writer.finish())
}

/// Whether the worker answered that its class loads hold one of the classes asked after.
pub(crate) fn decode_class_loads_response(payload: &[Value]) -> Result<bool, WorkerError> {
    let mut reader = message_reader(payload, CLASS_LOADS_RESPONSE)?;
    let holds = reader.read_bool("class loads answer").map_err(invalid_response)?;
    reader.finish().map_err(invalid_response)?;

    Ok(holds)
}

pub(crate) fn decode_collect_response(payload: &[Value]) -> Result<bool, WorkerError> {
    let mut reader = message_reader(payload, COLLECT_RESPONSE)?;
    let count = reader.read_count("worker reducers", MAXIMUM_REDUCERS).map_err(invalid_response)?;
    let mut previous = None;
    for _ in 0..count {
        let index = reader.read_u32("worker reducer extension index").map_err(invalid_response)?;
        if previous.is_some_and(|previous| previous >= index) {
            return Err(invalid_response("worker reducer extension indices are not strictly increasing"));
        }

        previous = Some(index);
        let _payload = reader.read_bytes("worker reducer payload").map_err(invalid_response)?;
    }

    reader.finish().map_err(invalid_response)?;
    Ok(count != 0)
}

/// The request that reduces the collect responses of every interpreter, each one a list of its own.
pub(crate) fn reduce_request(responses: &[Vec<Value>]) -> Result<Vec<Value>, WorkerError> {
    let mut writer = message_writer(REDUCE_REQUEST);
    writer.write_length(responses.len()).map_err(invalid_response)?;
    for response in responses {
        writer.write_value(Value::List(response.clone()));
    }

    Ok(writer.finish())
}

pub(crate) fn decode_reduce_response(payload: &[Value]) -> Result<(), WorkerError> {
    message_reader(payload, REDUCE_RESPONSE)?.finish().map_err(invalid_response)
}

fn message_writer(kind: u16) -> ValueWriter {
    let mut writer = ValueWriter::new();
    writer.write_value(Value::String(PROTOCOL.to_vec()));
    writer.write_u16(kind);
    writer
}

fn message_reader(payload: &[Value], expected_kind: u16) -> Result<ValueReader<'_>, WorkerError> {
    let mut reader = ValueReader::new(payload);
    if reader.read_bytes("worker management protocol").map_err(invalid_response)? != PROTOCOL {
        return Err(invalid_response("the message is no worker management message"));
    }

    let kind = reader.read_u16("worker management message kind").map_err(invalid_response)?;
    if kind != expected_kind {
        return Err(invalid_response(format!(
            "expected worker management message kind {expected_kind}, received {kind}"
        )));
    }

    Ok(reader)
}

fn invalid_response(error: impl std::fmt::Display) -> WorkerError {
    WorkerError::Reduction { message: error.to_string() }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn validates_collection_responses_and_encodes_the_ordered_batch() {
        let mut first = message_writer(COLLECT_RESPONSE);
        first.write_u32(1);
        first.write_u32(2);
        first.write_bytes(b"first").unwrap();
        let first = first.finish();
        let mut second = message_writer(COLLECT_RESPONSE);
        second.write_u32(1);
        second.write_u32(2);
        second.write_bytes(b"second").unwrap();
        let second = second.finish();

        assert!(decode_collect_response(&first).unwrap());
        assert!(decode_collect_response(&second).unwrap());

        let request = reduce_request(&[first.clone(), second.clone()]).unwrap();
        let mut reader = message_reader(&request, REDUCE_REQUEST).unwrap();
        assert_eq!(reader.read_u32("responses").unwrap(), 2);
        assert_eq!(reader.read_value("first").unwrap(), &Value::List(first));
        assert_eq!(reader.read_value("second").unwrap(), &Value::List(second));
        reader.finish().unwrap();
    }
}
