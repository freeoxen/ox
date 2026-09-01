# StructFS wire v1

This is a generic, carrier-independent request/response protocol for the
current StructFS `Reader` and `Writer` contracts. Conversation, worker, node,
SSH, and ox orchestration concepts are intentionally absent.

## Framing and deterministic encoding

A frame is a four-byte unsigned big-endian CBOR payload length followed by
exactly that many payload bytes. `max_frame_bytes` applies to the CBOR payload;
the four-byte prefix is additional. Multiple frames and partial reads belong to
the carrier layer.

Payloads use deterministic CBOR:

- definite lengths and shortest integer/length encodings;
- the shortest float width preserving the value, with canonical NaN `f97e00`;
- map keys ordered first by encoded-key length and then bytewise;
- no tags, undefined values, or unassigned wire discriminants;
- no duplicate or unknown map fields.

Decoders validate declared collection lengths, nesting, aggregate allocation,
and semantic shape before comparing the payload with its canonical re-encoding.
Structural frame/nesting/collection ceilings are symmetric: anything encoded
under a limit set can be decoded under that same set. `max_decoded_allocation`
is specifically a receiver-side budget for the temporary parse tree plus typed
conversion and is not an encode/decode symmetry promise.

## Message envelope

Every payload is an integer-keyed map. Keys are stable within v1.

| Key | Request | Response |
|---:|---|---|
| 0 | version (`1`) | version (`1`) |
| 1 | message kind (`0`) | message kind (`1`) |
| 2 | unsigned request ID | unsigned request ID |
| 3 | operation: read `0`, write `1` | status: success `0`, error `1` |
| 4 | path | successful result kind or typed error |
| 5 | write record, otherwise absent | successful record/path, if present |
| 6 | optional absolute Unix deadline (ms) | absent |

A read request must omit key 5. A write request must include it. A successful
read has result kind `0`; key 5 is absent when the path does not exist and is
present for every record—including `Parsed(Null)`. A successful write has
result kind `1` and key 5 is the StructFS path returned by `Writer::write`.

## Paths

A path is an array of UTF-8 strings. The decoder passes the array unchanged to
`Path::try_from_components`; it never parses and normalizes a slash-joined
string. Root is the empty array. Component count and component byte length are
bounded independently.

## Records and values

A record is an integer-keyed map:

- Raw: `{0: 0, 1: <bytes>, 2: <format string>}`
- Parsed: `{0: 1, 1: <Value>}`

Raw format is the exact `Format::as_str()` string, including custom formats.
Parsed values map directly to CBOR null, boolean, signed i64, preferred-width
float, text, bytes, array, and text-keyed map types. Text-keyed maps reject a
duplicate before insertion into the destination `BTreeMap`.

Ordinary finite `f64` values, infinities, and signed zero round-trip exactly.
Deterministic CBOR requires every NaN to use canonical `f97e00`, so NaN payload
and sign bits are intentionally normalized rather than preserved.

## Typed errors

An error response places `{0: <code>, 1: <diagnostic message>}` at envelope key
4. Stable codes are:

| Code | Category | Code | Category |
|---:|---|---:|---|
| 0 | invalid request | 6 | store |
| 1 | not found | 7 | disconnected |
| 2 | permission denied | 8 | internal |
| 3 | deadline exceeded | 9 | resource limit |
| 4 | overloaded | 10 | unsupported |
| 5 | conflict | | |

The category drives behavior; the message is diagnostic and must not be parsed
as a protocol discriminator.

## Versioning

Wire v1 is closed: an unsupported version, discriminant, or field is rejected.
A future version may define a different schema without making v1 peers silently
reinterpret data.

## Carrier contract

`RemoteStore` multiplexes requests by ID over one ordered bidirectional byte
stream; responses may arrive out of order. Its send queue and correlation table
are bounded. A local deadline removes correlation state, so a later response is
discarded by ID. Dropping a request future performs the same synchronous
cleanup.

Transport never retries a write. Timeout and disconnect are ambiguous outcomes
which the caller reconciles using its domain idempotency key. The server rejects
a request whose wire deadline is already expired, but after invoking the Store
it never cancels that operation merely because the deadline or connection has
ended. In particular, closing SSH/stdio/Unix response delivery cannot cancel an
admitted write.

Each server is constructed with one `ExportRoot`. Incoming relative paths are
joined beneath that root, and returned write paths must strip that exact root;
a sibling path or textual prefix collision is rejected. The Unix accept loop is
owned by the long-lived service. The stdio adapter carries bytes to that socket
without owning service or execution lifetime; stdin EOF half-closes the request
direction and drains outstanding responses before detaching.
