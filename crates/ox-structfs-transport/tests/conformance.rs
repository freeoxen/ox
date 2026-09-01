use std::collections::BTreeMap;

use bytes::Bytes;
use ox_structfs_transport::{
    CodecError, CodecLimits, Request, RequestOperation, Response, ResponseBody, WireCodec,
    WireError, WireErrorCode, WireMessage,
};
use proptest::prelude::*;
use structfs_core_store::{Format, Path, Record, Value};

fn fixture(source: &str) -> Vec<u8> {
    let source = source.trim();
    assert_eq!(source.len() % 2, 0);
    source
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}

fn read_request() -> WireMessage {
    WireMessage::Request(Request {
        request_id: 42,
        operation: RequestOperation::Read,
        path: Path::try_from_components(vec!["users".into(), "7".into()]).unwrap(),
        deadline_unix_ms: None,
    })
}

#[test]
fn canonical_frames_match_committed_byte_fixtures() {
    let codec = WireCodec::default();
    let cases = [
        (read_request(), include_str!("fixtures/read_request_v1.hex")),
        (
            WireMessage::Response(Response {
                request_id: 42,
                result: Ok(ResponseBody::Read(None)),
            }),
            include_str!("fixtures/read_missing_v1.hex"),
        ),
        (
            WireMessage::Response(Response {
                request_id: 42,
                result: Ok(ResponseBody::Read(Some(Record::parsed(Value::Null)))),
            }),
            include_str!("fixtures/read_parsed_null_v1.hex"),
        ),
        (
            WireMessage::Request(Request {
                request_id: 1,
                operation: RequestOperation::Write(Record::parsed(Value::Null)),
                path: Path::parse("data").unwrap(),
                deadline_unix_ms: Some(10),
            }),
            include_str!("fixtures/write_parsed_null_v1.hex"),
        ),
        (
            WireMessage::Response(Response {
                request_id: 1,
                result: Ok(ResponseBody::Write(Path::parse("handles/9").unwrap())),
            }),
            include_str!("fixtures/write_path_v1.hex"),
        ),
    ];

    for (message, expected) in cases {
        let expected = fixture(expected);
        assert_eq!(codec.encode(&message).unwrap(), expected);
        let decoded = codec.decode(&expected).unwrap();
        assert_eq!(codec.encode(&decoded).unwrap(), expected);
    }

    let missing = fixture(include_str!("fixtures/read_missing_v1.hex"));
    let WireMessage::Response(Response {
        result: Ok(ResponseBody::Read(missing)),
        ..
    }) = codec.decode(&missing).unwrap()
    else {
        panic!("expected successful read response")
    };
    assert!(missing.is_none());

    let parsed_null = fixture(include_str!("fixtures/read_parsed_null_v1.hex"));
    let WireMessage::Response(Response {
        result: Ok(ResponseBody::Read(Some(record))),
        ..
    }) = codec.decode(&parsed_null).unwrap()
    else {
        panic!("expected present read response")
    };
    assert_eq!(record.as_value(), Some(&Value::Null));
}

#[test]
fn every_value_variant_and_both_record_forms_round_trip_losslessly() {
    let codec = WireCodec::default();
    let mut map = BTreeMap::new();
    map.insert("z".into(), Value::Integer(i64::MIN));
    map.insert("aa".into(), Value::Float(1.5));
    let values = vec![
        Value::Null,
        Value::Bool(false),
        Value::Bool(true),
        Value::Integer(i64::MIN),
        Value::Integer(i64::MAX),
        Value::Float(-0.0),
        Value::Float(f64::INFINITY),
        Value::String("snowman ☃".into()),
        Value::Bytes(vec![0, 1, 255]),
        Value::Array(vec![Value::Null, Value::Integer(7)]),
        Value::Map(map),
    ];

    for (request_id, value) in values.into_iter().enumerate() {
        let expected = value.clone();
        let message = WireMessage::Request(Request {
            request_id: request_id as u64,
            operation: RequestOperation::Write(Record::parsed(value)),
            path: Path::parse("data").unwrap(),
            deadline_unix_ms: Some(4_000_000_000),
        });
        let bytes = codec.encode(&message).unwrap();
        let decoded = codec.decode(&bytes).unwrap();
        let WireMessage::Request(Request {
            operation: RequestOperation::Write(record),
            ..
        }) = &decoded
        else {
            panic!("expected write request")
        };
        assert_eq!(record.as_value(), Some(&expected));
        if let (Some(Value::Float(decoded)), Value::Float(expected)) = (record.as_value(), expected)
        {
            assert_eq!(decoded.to_bits(), expected.to_bits());
        }
        assert_eq!(codec.encode(&decoded).unwrap(), bytes);
    }

    let raw = WireMessage::Request(Request {
        request_id: 99,
        operation: RequestOperation::Write(Record::raw(
            Bytes::from_static(&[0, 159, 146, 150]),
            Format::new("application/x-custom+binary"),
        )),
        path: Path::parse("raw").unwrap(),
        deadline_unix_ms: None,
    });
    let bytes = codec.encode(&raw).unwrap();
    let WireMessage::Request(decoded) = codec.decode(&bytes).unwrap() else {
        panic!("expected request")
    };
    let RequestOperation::Write(record) = decoded.operation else {
        panic!("expected write")
    };
    assert!(record.is_raw());
    assert_eq!(record.as_bytes().unwrap().as_ref(), &[0, 159, 146, 150]);
    assert_eq!(record.format().as_str(), "application/x-custom+binary");
}

#[test]
fn typed_error_round_trips() {
    let codec = WireCodec::default();
    for (request_id, code) in [
        WireErrorCode::InvalidRequest,
        WireErrorCode::NotFound,
        WireErrorCode::PermissionDenied,
        WireErrorCode::DeadlineExceeded,
        WireErrorCode::Overloaded,
        WireErrorCode::Conflict,
        WireErrorCode::Store,
        WireErrorCode::Disconnected,
        WireErrorCode::Internal,
        WireErrorCode::ResourceLimit,
        WireErrorCode::Unsupported,
    ]
    .into_iter()
    .enumerate()
    {
        let message = WireMessage::Response(Response {
            request_id: request_id as u64,
            result: Err(WireError {
                code,
                message: "diagnostic".into(),
            }),
        });
        let frame = codec.encode(&message).unwrap();
        let WireMessage::Response(response) = codec.decode(&frame).unwrap() else {
            panic!("expected response")
        };
        assert_eq!(response.result.unwrap_err().code, code);
    }
}

#[test]
fn rejects_duplicate_keys_before_structfs_map_insertion() {
    // Canonical request containing a Parsed map with the key "a" twice.
    let payload = fixture("a60001010002010301048005a2000101a2616101616102");
    assert!(matches!(
        WireCodec::default().decode_payload(&payload),
        Err(CodecError::DuplicateKey)
    ));
}

#[test]
fn rejects_invalid_path_and_unsupported_discriminants() {
    let invalid_path = fixture("a500010100020103000481686261642d6e616d65");
    assert!(matches!(
        WireCodec::default().decode_payload(&invalid_path),
        Err(CodecError::InvalidPath(_))
    ));

    let unsupported_operation = fixture("a50001010002182a030204826575736572736137");
    assert!(matches!(
        WireCodec::default().decode_payload(&unsupported_operation),
        Err(CodecError::UnsupportedVariant {
            kind: "operation",
            value: 2,
        })
    ));

    let unsupported_version = fixture("a500020100020103000480");
    assert!(matches!(
        WireCodec::default().decode_payload(&unsupported_version),
        Err(CodecError::UnsupportedVersion(2))
    ));

    let write_without_record = fixture("a500010100020103010480");
    assert!(matches!(
        WireCodec::default().decode_payload(&write_without_record),
        Err(CodecError::MissingWriteRecord)
    ));

    let read_with_record = fixture("a60001010002010300048005a2000101f6");
    assert!(matches!(
        WireCodec::default().decode_payload(&read_with_record),
        Err(CodecError::RecordOnRead)
    ));
}

#[test]
fn rejects_noncanonical_integer_length_map_order_and_float() {
    let codec = WireCodec::default();
    let noncanonical_integer = fixture("a5000101000219002a030004826575736572736137");
    let noncanonical_length = fixture("a50001010002182a03000498026575736572736137");
    let noncanonical_order = fixture("a50100000102182a030004826575736572736137");

    for payload in [
        noncanonical_integer,
        noncanonical_length,
        noncanonical_order,
    ] {
        assert!(matches!(
            codec.decode_payload(&payload),
            Err(CodecError::NonCanonical)
        ));
    }

    let float_message = WireMessage::Request(Request {
        request_id: 7,
        operation: RequestOperation::Write(Record::parsed(Value::Float(1.5))),
        path: Path::parse("data").unwrap(),
        deadline_unix_ms: None,
    });
    let canonical = codec.encode_payload(&float_message).unwrap();
    let position = canonical
        .windows(3)
        .position(|window| window == [0xf9, 0x3e, 0x00])
        .unwrap();
    let mut noncanonical_float = canonical[..position].to_vec();
    noncanonical_float.extend_from_slice(&[0xfa, 0x3f, 0xc0, 0x00, 0x00]);
    noncanonical_float.extend_from_slice(&canonical[position + 3..]);
    assert!(matches!(
        codec.decode_payload(&noncanonical_float),
        Err(CodecError::NonCanonical)
    ));
}

#[test]
fn structfs_map_keys_use_encoded_length_then_byte_order() {
    let codec = WireCodec::default();
    let value = Value::Map(BTreeMap::from([
        ("aa".into(), Value::Integer(2)),
        ("z".into(), Value::Integer(1)),
    ]));
    let message = WireMessage::Request(Request {
        request_id: 8,
        operation: RequestOperation::Write(Record::parsed(value)),
        path: Path::parse("data").unwrap(),
        deadline_unix_ms: None,
    });
    let canonical = codec.encode_payload(&message).unwrap();
    let canonical_map = fixture("a2617a0162616102");
    let position = canonical
        .windows(canonical_map.len())
        .position(|window| window == canonical_map)
        .expect("shorter encoded key precedes the longer key");

    let mut wrong_order = canonical[..position].to_vec();
    wrong_order.extend_from_slice(&fixture("a262616102617a01"));
    wrong_order.extend_from_slice(&canonical[position + canonical_map.len()..]);
    assert!(matches!(
        codec.decode_payload(&wrong_order),
        Err(CodecError::NonCanonical)
    ));
}

#[test]
fn preferred_float_width_matches_known_cbor_encodings() {
    let codec = WireCodec::default();
    let cases: [(f64, &[u8]); 8] = [
        (1.5, &[0xf9, 0x3e, 0x00]),
        (-0.0, &[0xf9, 0x80, 0x00]),
        (f64::INFINITY, &[0xf9, 0x7c, 0x00]),
        (f64::NAN, &[0xf9, 0x7e, 0x00]),
        (2_f64.powi(-24), &[0xf9, 0x00, 0x01]),
        (2_f64.powi(-14), &[0xf9, 0x04, 0x00]),
        (100_000.0, &[0xfa, 0x47, 0xc3, 0x50, 0x00]),
        (1.1, &[0xfb, 0x3f, 0xf1, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a]),
    ];
    for (request_id, (value, expected_suffix)) in cases.into_iter().enumerate() {
        let message = WireMessage::Request(Request {
            request_id: request_id as u64,
            operation: RequestOperation::Write(Record::parsed(Value::Float(value))),
            path: Path::parse("data").unwrap(),
            deadline_unix_ms: None,
        });
        assert!(
            codec
                .encode_payload(&message)
                .unwrap()
                .ends_with(expected_suffix)
        );
    }
}

#[test]
fn framing_and_resource_limits_fail_closed() {
    let codec = WireCodec::default();
    let frame = codec.encode(&read_request()).unwrap();
    assert!(matches!(
        codec.decode(&frame[..frame.len() - 1]),
        Err(CodecError::TruncatedFrame { .. })
    ));
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(matches!(
        codec.decode(&trailing),
        Err(CodecError::TrailingFrameBytes { trailing: 1 })
    ));

    let limits = CodecLimits {
        max_frame_bytes: 8,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::FrameTooLarge { .. })
    ));

    let limits = CodecLimits {
        max_decoded_allocation: 16,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::AllocationLimit { .. })
    ));

    let limits = CodecLimits {
        max_path_components: 1,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::PathLength { .. })
    ));

    let limits = CodecLimits {
        max_path_component_bytes: 4,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::PathComponentLength { .. })
    ));
}

#[test]
fn declared_collection_limits_are_checked_before_bodies() {
    let cases = [
        (
            CodecLimits {
                max_record_bytes: 4,
                ..CodecLimits::default()
            },
            fixture("5a00000005"),
            "byte string",
            5,
        ),
        (
            CodecLimits {
                max_string_bytes: 4,
                ..CodecLimits::default()
            },
            fixture("7a00000005"),
            "text string",
            5,
        ),
        (
            CodecLimits {
                max_array_entries: 1,
                ..CodecLimits::default()
            },
            fixture("9a00000002"),
            "array",
            2,
        ),
        (
            CodecLimits {
                max_map_entries: 1,
                ..CodecLimits::default()
            },
            fixture("ba00000002"),
            "map",
            2,
        ),
    ];
    for (limits, payload, expected_kind, expected_actual) in cases {
        assert!(matches!(
            WireCodec::new(limits).decode_payload(&payload),
            Err(CodecError::CollectionLimit { kind, actual, .. })
                if kind == expected_kind && actual == expected_actual
        ));
    }
}

#[test]
fn depth_and_nested_map_entry_limits_are_enforced() {
    let codec = WireCodec::default();
    let mut nested = Value::Null;
    for _ in 0..8 {
        nested = Value::Array(vec![nested]);
    }
    let deeply_nested = WireMessage::Request(Request {
        request_id: 1,
        operation: RequestOperation::Write(Record::parsed(nested)),
        path: Path::parse("data").unwrap(),
        deadline_unix_ms: None,
    });
    let frame = codec.encode(&deeply_nested).unwrap();
    let limits = CodecLimits {
        max_nesting: 5,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::NestingLimit { .. })
    ));

    let wide_map = Value::Map(
        (0..7)
            .map(|index| (format!("k{index}"), Value::Null))
            .collect(),
    );
    let message = WireMessage::Request(Request {
        request_id: 2,
        operation: RequestOperation::Write(Record::parsed(wide_map)),
        path: Path::parse("data").unwrap(),
        deadline_unix_ms: None,
    });
    let frame = codec.encode(&message).unwrap();
    let limits = CodecLimits {
        // The request envelope has six entries; the nested map has seven.
        max_map_entries: 6,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).decode(&frame),
        Err(CodecError::CollectionLimit {
            kind: "map",
            actual: 7,
            ..
        })
    ));
}

#[test]
fn structural_nesting_limits_are_encode_decode_symmetric() {
    let messages = [
        WireMessage::Request(Request {
            request_id: 1,
            operation: RequestOperation::Read,
            path: Path::parse("").unwrap(),
            deadline_unix_ms: None,
        }),
        WireMessage::Request(Request {
            request_id: 2,
            operation: RequestOperation::Read,
            path: Path::parse("data").unwrap(),
            deadline_unix_ms: None,
        }),
        WireMessage::Request(Request {
            request_id: 3,
            operation: RequestOperation::Write(Record::raw(
                b"raw".to_vec(),
                Format::new("application/x-test"),
            )),
            path: Path::parse("").unwrap(),
            deadline_unix_ms: None,
        }),
        WireMessage::Request(Request {
            request_id: 4,
            operation: RequestOperation::Write(Record::parsed(Value::Array(vec![Value::Array(
                vec![Value::Null],
            )]))),
            path: Path::parse("").unwrap(),
            deadline_unix_ms: None,
        }),
        WireMessage::Response(Response {
            request_id: 5,
            result: Ok(ResponseBody::Read(None)),
        }),
        WireMessage::Response(Response {
            request_id: 6,
            result: Ok(ResponseBody::Write(Path::parse("result").unwrap())),
        }),
        WireMessage::Response(Response {
            request_id: 7,
            result: Err(WireError {
                code: WireErrorCode::Store,
                message: "failed".into(),
            }),
        }),
    ];

    for max_nesting in 0..=6 {
        let codec = WireCodec::new(CodecLimits {
            max_nesting,
            ..CodecLimits::default()
        });
        for message in &messages {
            if let Ok(encoded) = codec.encode(message) {
                codec
                    .decode(&encoded)
                    .expect("every encodable structural shape must be decodable");
            }
        }
    }

    let depth_one = WireCodec::new(CodecLimits {
        max_nesting: 1,
        ..CodecLimits::default()
    });
    assert!(depth_one.encode(&messages[0]).is_ok());
    assert!(matches!(
        depth_one.encode(&messages[1]),
        Err(CodecError::NestingLimit { .. })
    ));
    assert!(depth_one.encode(&messages[4]).is_ok());
    assert!(matches!(
        depth_one.encode(&messages[6]),
        Err(CodecError::NestingLimit { .. })
    ));

    let four_map_entries = WireCodec::new(CodecLimits {
        max_map_entries: 4,
        ..CodecLimits::default()
    });
    assert!(matches!(
        four_map_entries.encode(&messages[4]),
        Err(CodecError::CollectionLimit {
            kind: "map",
            actual: 5,
            ..
        })
    ));
    let five_map_entries = WireCodec::new(CodecLimits {
        max_map_entries: 5,
        ..CodecLimits::default()
    });
    let encoded = five_map_entries.encode(&messages[4]).unwrap();
    five_map_entries.decode(&encoded).unwrap();

    let no_array_entries = WireCodec::new(CodecLimits {
        max_array_entries: 0,
        ..CodecLimits::default()
    });
    let encoded = no_array_entries.encode(&messages[0]).unwrap();
    no_array_entries.decode(&encoded).unwrap();
    assert!(matches!(
        no_array_entries.encode(&messages[1]),
        Err(CodecError::CollectionLimit {
            kind: "array",
            actual: 1,
            ..
        })
    ));
}

#[test]
fn unknown_missing_and_truncated_cbor_are_rejected() {
    let unknown = fixture("a6000101000201030004800700");
    assert!(matches!(
        WireCodec::default().decode_payload(&unknown),
        Err(CodecError::UnknownField(7))
    ));

    let missing_path = fixture("a40001010002010300");
    assert!(matches!(
        WireCodec::default().decode_payload(&missing_path),
        Err(CodecError::MissingField(4))
    ));

    assert!(matches!(
        WireCodec::default().decode_payload(&[0xa5]),
        Err(CodecError::InvalidCbor(message)) if message.contains("truncated")
    ));
}

#[test]
fn encode_size_is_rejected_before_output_allocation() {
    let message = WireMessage::Request(Request {
        request_id: 3,
        operation: RequestOperation::Write(Record::raw(vec![0; 128], Format::OCTET_STREAM)),
        path: Path::parse("data").unwrap(),
        deadline_unix_ms: None,
    });
    let limits = CodecLimits {
        max_frame_bytes: 64,
        ..CodecLimits::default()
    };
    assert!(matches!(
        WireCodec::new(limits).encode(&message),
        Err(CodecError::FrameTooLarge { .. })
    ));
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Integer),
        any::<u64>().prop_map(|bits| Value::Float(f64::from_bits(bits))),
        ".{0,24}".prop_map(Value::String),
        prop::collection::vec(any::<u8>(), 0..24).prop_map(Value::Bytes),
    ]
}

fn arb_value() -> impl Strategy<Value = Value> {
    arb_leaf().prop_recursive(5, 128, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::btree_map("[a-z]{1,8}", inner, 0..8).prop_map(Value::Map),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn parsed_values_have_stable_canonical_round_trip(value in arb_value()) {
        let codec = WireCodec::default();
        let message = WireMessage::Request(Request {
            request_id: 123,
            operation: RequestOperation::Write(Record::parsed(value)),
            path: Path::parse("property").unwrap(),
            deadline_unix_ms: Some(999),
        });
        let encoded = codec.encode(&message).unwrap();
        let exact_limit = CodecLimits {
            max_frame_bytes: encoded.len() - 4,
            ..CodecLimits::default()
        };
        prop_assert_eq!(WireCodec::new(exact_limit).encode(&message).unwrap(), encoded.clone());
        let decoded = codec.decode(&encoded).unwrap();
        prop_assert_eq!(codec.encode(&decoded).unwrap(), encoded);
    }

    #[test]
    fn tight_structural_limits_preserve_encode_decode_symmetry(
        value in arb_value(),
        max_nesting in 0_usize..12,
        max_map_entries in 0_usize..12,
        max_array_entries in 0_usize..12,
    ) {
        let codec = WireCodec::new(CodecLimits {
            max_nesting,
            max_map_entries,
            max_array_entries,
            ..CodecLimits::default()
        });
        let message = WireMessage::Request(Request {
            request_id: 456,
            operation: RequestOperation::Write(Record::parsed(value)),
            path: Path::parse("property").unwrap(),
            deadline_unix_ms: None,
        });
        if let Ok(encoded) = codec.encode(&message) {
            prop_assert!(codec.decode(&encoded).is_ok());
        }
    }

    #[test]
    fn arbitrary_payloads_never_panic_and_accepted_bytes_are_canonical(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let codec = WireCodec::default();
        if let Ok(message) = codec.decode_payload(&bytes) {
            prop_assert_eq!(codec.encode_payload(&message).unwrap(), bytes);
        }
    }
}
