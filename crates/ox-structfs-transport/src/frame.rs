//! Length framing and deterministic CBOR codec for wire v1.

use std::collections::BTreeMap;
use std::mem::size_of;

use bytes::Bytes;
use structfs_core_store::{Format, Path, Record, Value};

use crate::{
    CodecError, Request, RequestOperation, Response, ResponseBody, WireError, WireErrorCode,
    WireMessage,
};

pub const WIRE_VERSION: u64 = 1;
const LENGTH_PREFIX_BYTES: usize = 4;

/// Resource ceilings applied before a decoded message reaches any Store.
#[derive(Clone, Debug)]
pub struct CodecLimits {
    /// Maximum CBOR payload bytes; the four-byte prefix is not included.
    pub max_frame_bytes: usize,
    pub max_nesting: usize,
    pub max_path_components: usize,
    pub max_path_component_bytes: usize,
    pub max_map_entries: usize,
    pub max_array_entries: usize,
    pub max_string_bytes: usize,
    pub max_record_bytes: usize,
    pub max_decoded_allocation: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 1024 * 1024,
            max_nesting: 64,
            max_path_components: 128,
            max_path_component_bytes: 256,
            max_map_entries: 4096,
            max_array_entries: 4096,
            max_string_bytes: 256 * 1024,
            max_record_bytes: 768 * 1024,
            max_decoded_allocation: 2 * 1024 * 1024,
        }
    }
}

/// Sans-I/O frame codec with immutable resource limits.
#[derive(Clone, Debug, Default)]
pub struct WireCodec {
    limits: CodecLimits,
}

impl WireCodec {
    pub fn new(limits: CodecLimits) -> Self {
        Self { limits }
    }

    pub fn limits(&self) -> &CodecLimits {
        &self.limits
    }

    /// Encode one complete length-prefixed frame.
    pub fn encode(&self, message: &WireMessage) -> Result<Vec<u8>, CodecError> {
        validate_message(message, &self.limits)?;
        let encoded_size = ensure_encoded_size(message, self.limits.max_frame_bytes)?;
        ensure_frame_length(encoded_size)?;
        let payload = encode_payload(message)?;
        let len = u32::try_from(payload.len()).map_err(|_| CodecError::LengthOverflow)?;
        let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }

    /// Decode exactly one complete length-prefixed frame.
    pub fn decode(&self, frame: &[u8]) -> Result<WireMessage, CodecError> {
        if frame.len() < LENGTH_PREFIX_BYTES {
            return Err(CodecError::TruncatedFrame {
                declared: LENGTH_PREFIX_BYTES,
                available: frame.len(),
            });
        }
        let declared =
            u32::from_be_bytes(frame[..4].try_into().expect("four-byte prefix")) as usize;
        if declared > self.limits.max_frame_bytes {
            return Err(CodecError::FrameTooLarge {
                actual: declared,
                limit: self.limits.max_frame_bytes,
            });
        }
        let available = frame.len() - LENGTH_PREFIX_BYTES;
        if available < declared {
            return Err(CodecError::TruncatedFrame {
                declared,
                available,
            });
        }
        if available > declared {
            return Err(CodecError::TrailingFrameBytes {
                trailing: available - declared,
            });
        }
        self.decode_payload(&frame[LENGTH_PREFIX_BYTES..])
    }

    /// Decode a payload whose carrier already consumed the length prefix.
    pub fn decode_payload(&self, payload: &[u8]) -> Result<WireMessage, CodecError> {
        if payload.len() > self.limits.max_frame_bytes {
            return Err(CodecError::FrameTooLarge {
                actual: payload.len(),
                limit: self.limits.max_frame_bytes,
            });
        }
        let mut parser = Parser::new(payload, &self.limits);
        let cbor = parser.parse_value(0)?;
        if parser.position != payload.len() {
            return Err(CodecError::InvalidCbor("trailing CBOR data".into()));
        }
        let mut budget = DecodeBudget {
            limits: &self.limits,
            allocated: parser.allocated,
        };
        let message = decode_message(cbor, &mut budget)?;

        // Bounds and duplicate checks have already happened. Comparing with
        // our deterministic encoder now rejects non-shortest integers/lengths,
        // non-preferred floats, and noncanonical map ordering.
        let canonical = encode_payload(&message)?;
        if canonical != payload {
            return Err(CodecError::NonCanonical);
        }
        Ok(message)
    }

    /// Encode a payload without the carrier length prefix.
    pub fn encode_payload(&self, message: &WireMessage) -> Result<Vec<u8>, CodecError> {
        validate_message(message, &self.limits)?;
        ensure_encoded_size(message, self.limits.max_frame_bytes)?;
        let payload = encode_payload(message)?;
        Ok(payload)
    }
}

#[derive(Debug)]
enum Cbor {
    Unsigned(u64),
    Negative(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
    Float(f64),
}

struct Parser<'a> {
    input: &'a [u8],
    position: usize,
    allocated: usize,
    limits: &'a CodecLimits,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8], limits: &'a CodecLimits) -> Self {
        Self {
            input,
            position: 0,
            allocated: 0,
            limits,
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Cbor, CodecError> {
        if depth > self.limits.max_nesting {
            return Err(CodecError::NestingLimit {
                limit: self.limits.max_nesting,
            });
        }
        let initial = self.byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;
        match major {
            0 => Ok(Cbor::Unsigned(self.argument(additional)?)),
            1 => {
                let argument = self.argument(additional)?;
                if argument > i64::MAX as u64 {
                    return Err(CodecError::InvalidCbor(
                        "negative integer is below i64::MIN".into(),
                    ));
                }
                Ok(Cbor::Negative(-1 - argument as i64))
            }
            2 => {
                let len =
                    self.declared_len(additional, "byte string", self.limits.max_record_bytes)?;
                self.charge(len)?;
                Ok(Cbor::Bytes(self.take(len)?.to_vec()))
            }
            3 => {
                let len =
                    self.declared_len(additional, "text string", self.limits.max_string_bytes)?;
                self.charge(len)?;
                let bytes = self.take(len)?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| CodecError::InvalidCbor("text string is not UTF-8".into()))?;
                Ok(Cbor::Text(text.to_owned()))
            }
            4 => {
                let len = self.declared_len(additional, "array", self.limits.max_array_entries)?;
                self.charge_collection::<Cbor>(len)?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.parse_value(depth + 1)?);
                }
                Ok(Cbor::Array(values))
            }
            5 => {
                let len = self.declared_len(additional, "map", self.limits.max_map_entries)?;
                self.charge_collection::<(Cbor, Cbor)>(len)?;
                let mut entries = Vec::with_capacity(len);
                for _ in 0..len {
                    entries.push((self.parse_value(depth + 1)?, self.parse_value(depth + 1)?));
                }
                Ok(Cbor::Map(entries))
            }
            6 => Err(CodecError::InvalidCbor("CBOR tags are unsupported".into())),
            7 => self.parse_simple(additional),
            _ => unreachable!(),
        }
    }

    fn parse_simple(&mut self, additional: u8) -> Result<Cbor, CodecError> {
        match additional {
            20 => Ok(Cbor::Bool(false)),
            21 => Ok(Cbor::Bool(true)),
            22 => Ok(Cbor::Null),
            25 => Ok(Cbor::Float(half_to_f64(self.u16()?))),
            26 => Ok(Cbor::Float(f32::from_bits(self.u32()?) as f64)),
            27 => Ok(Cbor::Float(f64::from_bits(self.u64()?))),
            31 => Err(CodecError::InvalidCbor(
                "indefinite-length CBOR is unsupported".into(),
            )),
            _ => Err(CodecError::InvalidCbor(format!(
                "unsupported simple value {additional}"
            ))),
        }
    }

    fn declared_len(
        &mut self,
        additional: u8,
        kind: &'static str,
        limit: usize,
    ) -> Result<usize, CodecError> {
        let raw = self.argument(additional)?;
        let actual = usize::try_from(raw).map_err(|_| CodecError::CollectionLimit {
            kind,
            actual: usize::MAX,
            limit,
        })?;
        if actual > limit {
            return Err(CodecError::CollectionLimit {
                kind,
                actual,
                limit,
            });
        }
        Ok(actual)
    }

    fn argument(&mut self, additional: u8) -> Result<u64, CodecError> {
        match additional {
            0..=23 => Ok(additional as u64),
            24 => Ok(self.byte()? as u64),
            25 => Ok(self.u16()? as u64),
            26 => Ok(self.u32()? as u64),
            27 => self.u64(),
            31 => Err(CodecError::InvalidCbor(
                "indefinite-length CBOR is unsupported".into(),
            )),
            _ => Err(CodecError::InvalidCbor(
                "reserved CBOR additional information".into(),
            )),
        }
    }

    fn charge_collection<T>(&mut self, len: usize) -> Result<(), CodecError> {
        let bytes = len
            .checked_mul(size_of::<T>())
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        self.charge(bytes)
    }

    fn charge(&mut self, bytes: usize) -> Result<(), CodecError> {
        let next = self
            .allocated
            .checked_add(bytes)
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        if next > self.limits.max_decoded_allocation {
            return Err(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            });
        }
        self.allocated = next;
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| CodecError::InvalidCbor("length overflow".into()))?;
        let slice = self
            .input
            .get(self.position..end)
            .ok_or_else(|| CodecError::InvalidCbor("truncated CBOR item".into()))?;
        self.position = end;
        Ok(slice)
    }
}

struct DecodeBudget<'a> {
    limits: &'a CodecLimits,
    allocated: usize,
}

impl DecodeBudget<'_> {
    fn charge(&mut self, bytes: usize) -> Result<(), CodecError> {
        self.allocated = self
            .allocated
            .checked_add(bytes)
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        if self.allocated > self.limits.max_decoded_allocation {
            return Err(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            });
        }
        Ok(())
    }

    fn charge_vec<T>(&mut self, len: usize) -> Result<(), CodecError> {
        let bytes = len
            .checked_mul(size_of::<T>())
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        self.charge(bytes)
    }

    fn charge_tree_map<K, V>(&mut self, len: usize) -> Result<(), CodecError> {
        let per_entry = size_of::<(K, V)>()
            .checked_add(3 * size_of::<usize>())
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        let bytes = len
            .checked_mul(per_entry)
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        self.charge(bytes)
    }
}

fn decode_message(cbor: Cbor, budget: &mut DecodeBudget<'_>) -> Result<WireMessage, CodecError> {
    let mut fields = into_uint_fields(cbor, budget)?;
    let version = take_u64(&mut fields, 0, "version")?;
    if version != WIRE_VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    let kind = take_u64(&mut fields, 1, "message kind")?;
    let request_id = take_u64(&mut fields, 2, "request id")?;
    match kind {
        0 => decode_request(fields, request_id, budget),
        1 => decode_response(fields, request_id, budget),
        value => Err(CodecError::UnsupportedVariant {
            kind: "message",
            value,
        }),
    }
}

fn decode_request(
    mut fields: BTreeMap<u64, Cbor>,
    request_id: u64,
    budget: &mut DecodeBudget<'_>,
) -> Result<WireMessage, CodecError> {
    let operation = take_u64(&mut fields, 3, "operation")?;
    let path = decode_path(take_field(&mut fields, 4)?, budget)?;
    let record = fields
        .remove(&5)
        .map(|value| decode_record(value, budget))
        .transpose()?;
    let deadline_unix_ms = fields
        .remove(&6)
        .map(|value| expect_u64(value, "deadline"))
        .transpose()?;
    ensure_no_fields(fields)?;
    let operation = match operation {
        0 if record.is_none() => RequestOperation::Read,
        0 => return Err(CodecError::RecordOnRead),
        1 => RequestOperation::Write(record.ok_or(CodecError::MissingWriteRecord)?),
        value => {
            return Err(CodecError::UnsupportedVariant {
                kind: "operation",
                value,
            });
        }
    };
    Ok(WireMessage::Request(Request {
        request_id,
        operation,
        path,
        deadline_unix_ms,
    }))
}

fn decode_response(
    mut fields: BTreeMap<u64, Cbor>,
    request_id: u64,
    budget: &mut DecodeBudget<'_>,
) -> Result<WireMessage, CodecError> {
    let status = take_u64(&mut fields, 3, "response status")?;
    let result = match status {
        0 => {
            let kind = take_u64(&mut fields, 4, "response kind")?;
            match kind {
                0 => {
                    let record = fields
                        .remove(&5)
                        .map(|value| decode_record(value, budget))
                        .transpose()?;
                    Ok(ResponseBody::Read(record))
                }
                1 => Ok(ResponseBody::Write(decode_path(
                    take_field(&mut fields, 5)?,
                    budget,
                )?)),
                value => {
                    return Err(CodecError::UnsupportedVariant {
                        kind: "response",
                        value,
                    });
                }
            }
        }
        1 => Err(decode_wire_error(take_field(&mut fields, 4)?, budget)?),
        value => {
            return Err(CodecError::UnsupportedVariant {
                kind: "response status",
                value,
            });
        }
    };
    ensure_no_fields(fields)?;
    Ok(WireMessage::Response(Response { request_id, result }))
}

fn decode_wire_error(cbor: Cbor, budget: &mut DecodeBudget<'_>) -> Result<WireError, CodecError> {
    let mut fields = into_uint_fields(cbor, budget)?;
    let raw_code = take_u64(&mut fields, 0, "error code")?;
    let code = match raw_code {
        0 => WireErrorCode::InvalidRequest,
        1 => WireErrorCode::NotFound,
        2 => WireErrorCode::PermissionDenied,
        3 => WireErrorCode::DeadlineExceeded,
        4 => WireErrorCode::Overloaded,
        5 => WireErrorCode::Conflict,
        6 => WireErrorCode::Store,
        7 => WireErrorCode::Disconnected,
        8 => WireErrorCode::Internal,
        9 => WireErrorCode::ResourceLimit,
        10 => WireErrorCode::Unsupported,
        value => {
            return Err(CodecError::UnsupportedVariant {
                kind: "error code",
                value,
            });
        }
    };
    let message = expect_text(take_field(&mut fields, 1)?, "error message")?;
    ensure_no_fields(fields)?;
    Ok(WireError { code, message })
}

fn decode_path(cbor: Cbor, budget: &mut DecodeBudget<'_>) -> Result<Path, CodecError> {
    let Cbor::Array(values) = cbor else {
        return Err(CodecError::WrongType {
            field: "path",
            expected: "array",
        });
    };
    if values.len() > budget.limits.max_path_components {
        return Err(CodecError::PathLength {
            actual: values.len(),
            limit: budget.limits.max_path_components,
        });
    }
    budget.charge_vec::<String>(values.len())?;
    let mut components = Vec::with_capacity(values.len());
    for value in values {
        let component = expect_text(value, "path component")?;
        if component.len() > budget.limits.max_path_component_bytes {
            return Err(CodecError::PathComponentLength {
                actual: component.len(),
                limit: budget.limits.max_path_component_bytes,
            });
        }
        components.push(component);
    }
    Path::try_from_components(components)
        .map_err(|error| CodecError::InvalidPath(error.to_string()))
}

fn decode_record(cbor: Cbor, budget: &mut DecodeBudget<'_>) -> Result<Record, CodecError> {
    let mut fields = into_uint_fields(cbor, budget)?;
    let kind = take_u64(&mut fields, 0, "record kind")?;
    let record = match kind {
        0 => {
            let bytes = expect_bytes(take_field(&mut fields, 1)?, "record bytes")?;
            if bytes.len() > budget.limits.max_record_bytes {
                return Err(CodecError::CollectionLimit {
                    kind: "record bytes",
                    actual: bytes.len(),
                    limit: budget.limits.max_record_bytes,
                });
            }
            let format = expect_text(take_field(&mut fields, 2)?, "record format")?;
            Record::raw(Bytes::from(bytes), Format::new(format))
        }
        1 => Record::parsed(decode_value(take_field(&mut fields, 1)?, budget)?),
        value => {
            return Err(CodecError::UnsupportedVariant {
                kind: "record",
                value,
            });
        }
    };
    ensure_no_fields(fields)?;
    Ok(record)
}

fn decode_value(cbor: Cbor, budget: &mut DecodeBudget<'_>) -> Result<Value, CodecError> {
    match cbor {
        Cbor::Unsigned(value) => i64::try_from(value)
            .map(Value::Integer)
            .map_err(|_| CodecError::InvalidCbor("StructFS integer exceeds i64::MAX".into())),
        Cbor::Negative(value) => Ok(Value::Integer(value)),
        Cbor::Bytes(value) => Ok(Value::Bytes(value)),
        Cbor::Text(value) => Ok(Value::String(value)),
        Cbor::Array(values) => {
            budget.charge_vec::<Value>(values.len())?;
            let mut decoded = Vec::with_capacity(values.len());
            for value in values {
                decoded.push(decode_value(value, budget)?);
            }
            Ok(Value::Array(decoded))
        }
        Cbor::Map(entries) => {
            if entries.len() > budget.limits.max_map_entries {
                return Err(CodecError::CollectionLimit {
                    kind: "map",
                    actual: entries.len(),
                    limit: budget.limits.max_map_entries,
                });
            }
            budget.charge_tree_map::<String, Value>(entries.len())?;
            let mut map = BTreeMap::new();
            for (key, value) in entries {
                let key = expect_text(key, "StructFS map key")?;
                // Detect before BTreeMap insertion so a duplicate never
                // silently overwrites the first decoded value.
                if map.contains_key(&key) {
                    return Err(CodecError::DuplicateKey);
                }
                map.insert(key, decode_value(value, budget)?);
            }
            Ok(Value::Map(map))
        }
        Cbor::Bool(value) => Ok(Value::Bool(value)),
        Cbor::Null => Ok(Value::Null),
        Cbor::Float(value) => Ok(Value::Float(value)),
    }
}

fn into_uint_fields(
    cbor: Cbor,
    budget: &mut DecodeBudget<'_>,
) -> Result<BTreeMap<u64, Cbor>, CodecError> {
    let Cbor::Map(entries) = cbor else {
        return Err(CodecError::WrongType {
            field: "wire object",
            expected: "map",
        });
    };
    budget.charge_tree_map::<u64, Cbor>(entries.len())?;
    let mut fields = BTreeMap::new();
    for (key, value) in entries {
        let Cbor::Unsigned(key) = key else {
            return Err(CodecError::WrongType {
                field: "wire object key",
                expected: "unsigned integer",
            });
        };
        if fields.contains_key(&key) {
            return Err(CodecError::DuplicateKey);
        }
        fields.insert(key, value);
    }
    Ok(fields)
}

fn take_field(fields: &mut BTreeMap<u64, Cbor>, field: u64) -> Result<Cbor, CodecError> {
    fields.remove(&field).ok_or(CodecError::MissingField(field))
}

fn take_u64(
    fields: &mut BTreeMap<u64, Cbor>,
    field: u64,
    name: &'static str,
) -> Result<u64, CodecError> {
    expect_u64(take_field(fields, field)?, name)
}

fn expect_u64(value: Cbor, field: &'static str) -> Result<u64, CodecError> {
    match value {
        Cbor::Unsigned(value) => Ok(value),
        _ => Err(CodecError::WrongType {
            field,
            expected: "unsigned integer",
        }),
    }
}

fn expect_text(value: Cbor, field: &'static str) -> Result<String, CodecError> {
    match value {
        Cbor::Text(value) => Ok(value),
        _ => Err(CodecError::WrongType {
            field,
            expected: "text string",
        }),
    }
}

fn expect_bytes(value: Cbor, field: &'static str) -> Result<Vec<u8>, CodecError> {
    match value {
        Cbor::Bytes(value) => Ok(value),
        _ => Err(CodecError::WrongType {
            field,
            expected: "byte string",
        }),
    }
}

fn ensure_no_fields(fields: BTreeMap<u64, Cbor>) -> Result<(), CodecError> {
    match fields.into_keys().next() {
        Some(field) => Err(CodecError::UnknownField(field)),
        None => Ok(()),
    }
}

fn encode_payload(message: &WireMessage) -> Result<Vec<u8>, CodecError> {
    let mut output = Vec::new();
    match message {
        WireMessage::Request(request) => encode_request(request, &mut output)?,
        WireMessage::Response(response) => encode_response(response, &mut output)?,
    }
    Ok(output)
}

fn ensure_encoded_size(message: &WireMessage, limit: usize) -> Result<usize, CodecError> {
    let actual = encoded_message_size(message)?;
    if actual > limit {
        return Err(CodecError::FrameTooLarge { actual, limit });
    }
    Ok(actual)
}

fn ensure_frame_length(actual: usize) -> Result<u32, CodecError> {
    u32::try_from(actual).map_err(|_| CodecError::LengthOverflow)
}

fn encoded_message_size(message: &WireMessage) -> Result<usize, CodecError> {
    match message {
        WireMessage::Request(request) => {
            let record = match &request.operation {
                RequestOperation::Read => None,
                RequestOperation::Write(record) => Some(encoded_record_size(record)?),
            };
            let count =
                5 + usize::from(record.is_some()) + usize::from(request.deadline_unix_ms.is_some());
            checked_size_sum(
                [
                    major_argument_size(count as u64),
                    1 + major_argument_size(WIRE_VERSION),
                    1 + 1,
                    1 + major_argument_size(request.request_id),
                    1 + 1,
                    1 + encoded_path_size(&request.path),
                    record.map_or(0, |size| 1 + size),
                    request
                        .deadline_unix_ms
                        .map_or(0, |deadline| 1 + major_argument_size(deadline)),
                ]
                .into_iter(),
            )
        }
        WireMessage::Response(response) => match &response.result {
            Ok(ResponseBody::Read(record)) => checked_size_sum(
                [
                    major_argument_size((5 + usize::from(record.is_some())) as u64),
                    encoded_response_header_size(response.request_id),
                    1 + 1,
                    match record {
                        Some(record) => 1 + encoded_record_size(record)?,
                        None => 0,
                    },
                ]
                .into_iter(),
            ),
            Ok(ResponseBody::Write(path)) => checked_size_sum(
                [
                    major_argument_size(6),
                    encoded_response_header_size(response.request_id),
                    1 + 1,
                    1 + encoded_path_size(path),
                ]
                .into_iter(),
            ),
            Err(error) => checked_size_sum(
                [
                    major_argument_size(5),
                    encoded_response_header_size(response.request_id),
                    1 + encoded_wire_error_size(error),
                ]
                .into_iter(),
            ),
        },
    }
}

fn encoded_response_header_size(request_id: u64) -> usize {
    // Keys 0 through 3 and the version/kind/status values each fit one byte.
    7 + major_argument_size(request_id)
}

fn encoded_wire_error_size(error: &WireError) -> usize {
    major_argument_size(2)
        + 1
        + major_argument_size(error.code as u64)
        + 1
        + encoded_text_size(&error.message)
}

fn encoded_path_size(path: &Path) -> usize {
    major_argument_size(path.len() as u64)
        + path
            .iter()
            .map(|component| encoded_text_size(component))
            .sum::<usize>()
}

fn encoded_record_size(record: &Record) -> Result<usize, CodecError> {
    match record {
        Record::Raw { bytes, format, .. } => checked_size_sum(
            [
                major_argument_size(3),
                1 + 1,
                1 + major_argument_size(bytes.len() as u64) + bytes.len(),
                1 + encoded_text_size(format.as_str()),
            ]
            .into_iter(),
        ),
        Record::Parsed(value) => checked_size_sum(
            [
                major_argument_size(2),
                1 + 1,
                1 + encoded_value_size(value)?,
            ]
            .into_iter(),
        ),
        _ => Err(CodecError::UnsupportedStructFsVariant),
    }
}

fn encoded_value_size(value: &Value) -> Result<usize, CodecError> {
    match value {
        Value::Null | Value::Bool(_) => Ok(1),
        Value::Integer(value) if *value >= 0 => Ok(major_argument_size(*value as u64)),
        Value::Integer(value) => Ok(major_argument_size((-1_i128 - *value as i128) as u64)),
        Value::Float(value) if value.is_nan() || exact_f16_bits(*value).is_some() => Ok(3),
        Value::Float(value) if exact_f32(*value) => Ok(5),
        Value::Float(_) => Ok(9),
        Value::String(value) => Ok(encoded_text_size(value)),
        Value::Bytes(value) => major_argument_size(value.len() as u64)
            .checked_add(value.len())
            .ok_or(CodecError::LengthOverflow),
        Value::Array(values) => {
            let mut total = major_argument_size(values.len() as u64);
            for value in values {
                total = total
                    .checked_add(encoded_value_size(value)?)
                    .ok_or(CodecError::LengthOverflow)?;
            }
            Ok(total)
        }
        Value::Map(values) => {
            let mut total = major_argument_size(values.len() as u64);
            for (key, value) in values {
                let value_size = encoded_value_size(value)?;
                total = total
                    .checked_add(encoded_text_size(key))
                    .and_then(|total| total.checked_add(value_size))
                    .ok_or(CodecError::LengthOverflow)?;
            }
            Ok(total)
        }
        _ => Err(CodecError::UnsupportedStructFsVariant),
    }
}

fn encoded_text_size(value: &str) -> usize {
    major_argument_size(value.len() as u64) + value.len()
}

fn major_argument_size(argument: u64) -> usize {
    match argument {
        0..=23 => 1,
        24..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

fn checked_size_sum(mut sizes: impl Iterator<Item = usize>) -> Result<usize, CodecError> {
    sizes.try_fold(0_usize, |total, size| {
        total.checked_add(size).ok_or(CodecError::LengthOverflow)
    })
}

fn encode_request(request: &Request, output: &mut Vec<u8>) -> Result<(), CodecError> {
    let has_record = matches!(request.operation, RequestOperation::Write(_));
    encode_map_len(
        5 + usize::from(has_record) + usize::from(request.deadline_unix_ms.is_some()),
        output,
    );
    encode_uint(0, output);
    encode_uint(WIRE_VERSION, output);
    encode_uint(1, output);
    encode_uint(0, output);
    encode_uint(2, output);
    encode_uint(request.request_id, output);
    encode_uint(3, output);
    encode_uint(
        match request.operation {
            RequestOperation::Read => 0,
            RequestOperation::Write(_) => 1,
        },
        output,
    );
    encode_uint(4, output);
    encode_path(&request.path, output);
    if let RequestOperation::Write(record) = &request.operation {
        encode_uint(5, output);
        encode_record(record, output)?;
    }
    if let Some(deadline) = request.deadline_unix_ms {
        encode_uint(6, output);
        encode_uint(deadline, output);
    }
    Ok(())
}

fn encode_response(response: &Response, output: &mut Vec<u8>) -> Result<(), CodecError> {
    match &response.result {
        Ok(ResponseBody::Read(record)) => {
            encode_map_len(5 + usize::from(record.is_some()), output);
            encode_response_header(response.request_id, 0, output);
            encode_uint(4, output);
            encode_uint(0, output);
            if let Some(record) = record {
                encode_uint(5, output);
                encode_record(record, output)?;
            }
        }
        Ok(ResponseBody::Write(path)) => {
            encode_map_len(6, output);
            encode_response_header(response.request_id, 0, output);
            encode_uint(4, output);
            encode_uint(1, output);
            encode_uint(5, output);
            encode_path(path, output);
        }
        Err(error) => {
            encode_map_len(5, output);
            encode_response_header(response.request_id, 1, output);
            encode_uint(4, output);
            encode_map_len(2, output);
            encode_uint(0, output);
            encode_uint(error.code as u64, output);
            encode_uint(1, output);
            encode_text(&error.message, output);
        }
    }
    Ok(())
}

fn encode_response_header(request_id: u64, status: u64, output: &mut Vec<u8>) {
    encode_uint(0, output);
    encode_uint(WIRE_VERSION, output);
    encode_uint(1, output);
    encode_uint(1, output);
    encode_uint(2, output);
    encode_uint(request_id, output);
    encode_uint(3, output);
    encode_uint(status, output);
}

fn encode_path(path: &Path, output: &mut Vec<u8>) {
    encode_array_len(path.len(), output);
    for component in path.iter() {
        encode_text(component, output);
    }
}

fn encode_record(record: &Record, output: &mut Vec<u8>) -> Result<(), CodecError> {
    match record {
        Record::Raw { bytes, format, .. } => {
            encode_map_len(3, output);
            encode_uint(0, output);
            encode_uint(0, output);
            encode_uint(1, output);
            encode_bytes(bytes, output);
            encode_uint(2, output);
            encode_text(format.as_str(), output);
        }
        Record::Parsed(value) => {
            encode_map_len(2, output);
            encode_uint(0, output);
            encode_uint(1, output);
            encode_uint(1, output);
            encode_value(value, output)?;
        }
        _ => return Err(CodecError::UnsupportedStructFsVariant),
    }
    Ok(())
}

fn encode_value(value: &Value, output: &mut Vec<u8>) -> Result<(), CodecError> {
    match value {
        Value::Null => output.push(0xf6),
        Value::Bool(false) => output.push(0xf4),
        Value::Bool(true) => output.push(0xf5),
        Value::Integer(value) if *value >= 0 => encode_uint(*value as u64, output),
        Value::Integer(value) => encode_negative(*value, output),
        Value::Float(value) => encode_float(*value, output),
        Value::String(value) => encode_text(value, output),
        Value::Bytes(value) => encode_bytes(value, output),
        Value::Array(values) => {
            encode_array_len(values.len(), output);
            for value in values {
                encode_value(value, output)?;
            }
        }
        Value::Map(values) => {
            encode_map_len(values.len(), output);
            let mut entries = values
                .iter()
                .map(|(key, value)| {
                    let mut encoded_key = Vec::new();
                    encode_text(key, &mut encoded_key);
                    (encoded_key, value)
                })
                .collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| {
                left.len().cmp(&right.len()).then_with(|| left.cmp(right))
            });
            for (key, value) in entries {
                output.extend_from_slice(&key);
                encode_value(value, output)?;
            }
        }
        _ => return Err(CodecError::UnsupportedStructFsVariant),
    }
    Ok(())
}

fn encode_uint(value: u64, output: &mut Vec<u8>) {
    encode_major_argument(0, value, output);
}

fn encode_negative(value: i64, output: &mut Vec<u8>) {
    let argument = (-1_i128 - value as i128) as u64;
    encode_major_argument(1, argument, output);
}

fn encode_bytes(value: &[u8], output: &mut Vec<u8>) {
    encode_major_argument(2, value.len() as u64, output);
    output.extend_from_slice(value);
}

fn encode_text(value: &str, output: &mut Vec<u8>) {
    encode_major_argument(3, value.len() as u64, output);
    output.extend_from_slice(value.as_bytes());
}

fn encode_array_len(len: usize, output: &mut Vec<u8>) {
    encode_major_argument(4, len as u64, output);
}

fn encode_map_len(len: usize, output: &mut Vec<u8>) {
    encode_major_argument(5, len as u64, output);
}

fn encode_major_argument(major: u8, argument: u64, output: &mut Vec<u8>) {
    let prefix = major << 5;
    match argument {
        0..=23 => output.push(prefix | argument as u8),
        24..=0xff => output.extend_from_slice(&[prefix | 24, argument as u8]),
        0x100..=0xffff => {
            output.push(prefix | 25);
            output.extend_from_slice(&(argument as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            output.push(prefix | 26);
            output.extend_from_slice(&(argument as u32).to_be_bytes());
        }
        _ => {
            output.push(prefix | 27);
            output.extend_from_slice(&argument.to_be_bytes());
        }
    }
}

fn encode_float(value: f64, output: &mut Vec<u8>) {
    if value.is_nan() {
        output.extend_from_slice(&[0xf9, 0x7e, 0x00]);
        return;
    }
    if let Some(bits) = exact_f16_bits(value) {
        output.push(0xf9);
        output.extend_from_slice(&bits.to_be_bytes());
    } else if exact_f32(value) {
        output.push(0xfa);
        output.extend_from_slice(&(value as f32).to_bits().to_be_bytes());
    } else {
        output.push(0xfb);
        output.extend_from_slice(&value.to_bits().to_be_bytes());
    }
}

fn exact_f32(value: f64) -> bool {
    let converted = value as f32;
    converted as f64 == value
        && (!value.eq(&0.0) || converted.is_sign_negative() == value.is_sign_negative())
}

fn exact_f16_bits(value: f64) -> Option<u16> {
    if !exact_f32(value) {
        return None;
    }
    let bits = f32_to_f16_bits(value as f32);
    let restored = half_to_f64(bits);
    if restored == value
        && (!value.eq(&0.0) || restored.is_sign_negative() == value.is_sign_negative())
    {
        Some(bits)
    } else {
        None
    }
}

fn half_to_f64(bits: u16) -> f64 {
    let sign = ((bits as u32) & 0x8000) << 16;
    let exponent = ((bits >> 10) & 0x1f) as u32;
    let fraction = (bits & 0x03ff) as u32;
    let float_bits = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let mut fraction = fraction;
            let mut exponent = 113_u32;
            while fraction & 0x0400 == 0 {
                fraction <<= 1;
                exponent -= 1;
            }
            sign | (exponent << 23) | ((fraction & 0x03ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((exponent + 112) << 23) | (fraction << 13),
    };
    f32::from_bits(float_bits) as f64
}

fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32;
    let fraction = bits & 0x007f_ffff;
    if exponent == 255 {
        return sign | if fraction == 0 { 0x7c00 } else { 0x7e00 };
    }
    let half_exponent = exponent - 127 + 15;
    if half_exponent >= 31 {
        return sign | 0x7c00;
    }
    if half_exponent <= 0 {
        if half_exponent < -10 {
            return sign;
        }
        let mantissa = fraction | 0x0080_0000;
        let shift = 14 - half_exponent;
        let mut half = (mantissa >> shift) as u16;
        let remainder = mantissa & ((1_u32 << shift) - 1);
        let halfway = 1_u32 << (shift - 1);
        if remainder > halfway || (remainder == halfway && half & 1 == 1) {
            half += 1;
        }
        return sign | half;
    }
    let mut half = ((half_exponent as u16) << 10) | (fraction >> 13) as u16;
    let remainder = fraction & 0x1fff;
    if remainder > 0x1000 || (remainder == 0x1000 && half & 1 == 1) {
        half += 1;
    }
    sign | half
}

struct ValidationBudget<'a> {
    limits: &'a CodecLimits,
    allocated: usize,
}

impl ValidationBudget<'_> {
    fn charge(&mut self, bytes: usize) -> Result<(), CodecError> {
        self.allocated = self
            .allocated
            .checked_add(bytes)
            .ok_or(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            })?;
        if self.allocated > self.limits.max_decoded_allocation {
            return Err(CodecError::AllocationLimit {
                limit: self.limits.max_decoded_allocation,
            });
        }
        Ok(())
    }
}

fn validate_message(message: &WireMessage, limits: &CodecLimits) -> Result<(), CodecError> {
    let mut budget = ValidationBudget {
        limits,
        allocated: 0,
    };
    // Every nonempty envelope has keys and values at CBOR depth 1.
    validate_depth(1, &budget)?;
    let envelope_entries = match message {
        WireMessage::Request(request) => {
            5 + usize::from(matches!(request.operation, RequestOperation::Write(_)))
                + usize::from(request.deadline_unix_ms.is_some())
        }
        WireMessage::Response(response) => match &response.result {
            Ok(ResponseBody::Read(record)) => 5 + usize::from(record.is_some()),
            Ok(ResponseBody::Write(_)) => 6,
            Err(_) => 5,
        },
    };
    validate_map_entries(envelope_entries, &budget)?;
    match message {
        WireMessage::Request(request) => {
            validate_path(&request.path, &mut budget)?;
            if let RequestOperation::Write(record) = &request.operation {
                validate_record(record, &mut budget)?;
            }
        }
        WireMessage::Response(response) => match &response.result {
            Ok(ResponseBody::Read(Some(record))) => validate_record(record, &mut budget)?,
            Ok(ResponseBody::Read(None)) => {}
            Ok(ResponseBody::Write(path)) => validate_path(path, &mut budget)?,
            Err(error) => {
                // The error map is at depth 1; its code and message are at 2.
                validate_depth(2, &budget)?;
                validate_map_entries(2, &budget)?;
                validate_string(&error.message, &mut budget)?;
            }
        },
    }
    Ok(())
}

fn validate_path(path: &Path, budget: &mut ValidationBudget<'_>) -> Result<(), CodecError> {
    if path.len() > budget.limits.max_path_components {
        return Err(CodecError::PathLength {
            actual: path.len(),
            limit: budget.limits.max_path_components,
        });
    }
    validate_array_entries(path.len(), budget)?;
    budget.charge(path.len().saturating_mul(size_of::<String>()))?;
    if !path.is_empty() {
        // The path array is at depth 1 and each component is at depth 2.
        validate_depth(2, budget)?;
    }
    for component in path.iter() {
        if component.len() > budget.limits.max_path_component_bytes {
            return Err(CodecError::PathComponentLength {
                actual: component.len(),
                limit: budget.limits.max_path_component_bytes,
            });
        }
        validate_string(component, budget)?;
    }
    Ok(())
}

fn validate_record(record: &Record, budget: &mut ValidationBudget<'_>) -> Result<(), CodecError> {
    // The record map is at depth 1 and always has fields at depth 2.
    validate_depth(2, budget)?;
    match record {
        Record::Raw { bytes, format, .. } => {
            validate_map_entries(3, budget)?;
            if bytes.len() > budget.limits.max_record_bytes {
                return Err(CodecError::CollectionLimit {
                    kind: "record bytes",
                    actual: bytes.len(),
                    limit: budget.limits.max_record_bytes,
                });
            }
            budget.charge(bytes.len())?;
            validate_string(format.as_str(), budget)
        }
        Record::Parsed(value) => {
            validate_map_entries(2, budget)?;
            validate_value(value, 2, budget)
        }
        _ => Err(CodecError::UnsupportedStructFsVariant),
    }
}

fn validate_value(
    value: &Value,
    depth: usize,
    budget: &mut ValidationBudget<'_>,
) -> Result<(), CodecError> {
    validate_depth(depth, budget)?;
    match value {
        Value::Null | Value::Bool(_) | Value::Integer(_) | Value::Float(_) => Ok(()),
        Value::String(value) => validate_string(value, budget),
        Value::Bytes(value) => {
            if value.len() > budget.limits.max_record_bytes {
                return Err(CodecError::CollectionLimit {
                    kind: "byte string",
                    actual: value.len(),
                    limit: budget.limits.max_record_bytes,
                });
            }
            budget.charge(value.len())
        }
        Value::Array(values) => {
            validate_array_entries(values.len(), budget)?;
            budget.charge(values.len().saturating_mul(size_of::<Value>()))?;
            for value in values {
                validate_value(value, depth + 1, budget)?;
            }
            Ok(())
        }
        Value::Map(values) => {
            validate_map_entries(values.len(), budget)?;
            budget.charge(
                values
                    .len()
                    .saturating_mul(size_of::<String>() + size_of::<Value>()),
            )?;
            for (key, value) in values {
                validate_string(key, budget)?;
                validate_value(value, depth + 1, budget)?;
            }
            Ok(())
        }
        _ => Err(CodecError::UnsupportedStructFsVariant),
    }
}

fn validate_string(value: &str, budget: &mut ValidationBudget<'_>) -> Result<(), CodecError> {
    if value.len() > budget.limits.max_string_bytes {
        return Err(CodecError::CollectionLimit {
            kind: "text string",
            actual: value.len(),
            limit: budget.limits.max_string_bytes,
        });
    }
    budget.charge(value.len())
}

fn validate_depth(depth: usize, budget: &ValidationBudget<'_>) -> Result<(), CodecError> {
    if depth > budget.limits.max_nesting {
        return Err(CodecError::NestingLimit {
            limit: budget.limits.max_nesting,
        });
    }
    Ok(())
}

fn validate_map_entries(actual: usize, budget: &ValidationBudget<'_>) -> Result<(), CodecError> {
    if actual > budget.limits.max_map_entries {
        return Err(CodecError::CollectionLimit {
            kind: "map",
            actual,
            limit: budget.limits.max_map_entries,
        });
    }
    Ok(())
}

fn validate_array_entries(actual: usize, budget: &ValidationBudget<'_>) -> Result<(), CodecError> {
    if actual > budget.limits.max_array_entries {
        return Err(CodecError::CollectionLimit {
            kind: "array",
            actual,
            limit: budget.limits.max_array_entries,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn framed_length_overflow_is_rejected_by_arithmetic_preflight() {
        assert_eq!(
            ensure_frame_length(u32::MAX as usize + 1),
            Err(CodecError::LengthOverflow)
        );
    }
}
