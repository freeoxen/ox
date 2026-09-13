# Feedback from migrating Ox to StructFS / Featherweight 0.2.0

Draft for the StructFS and Featherweight maintainers — September 13, 2026.

Hello,

We've been moving Ox onto the published 0.2.0 crates and wanted to share what
has removed work for us, and what would let us delete more integration code.
Our gateway now uses Featherweight's guest SDK, assembly namespaces and async
execution. We replaced our path proc macro, store combinators and cancellation
token with upstream implementations. We're also using Shared<MemoryStore> for
codec jobs, PathTrie for broker lookup, and the standard detached store traits.

The release covers substantially more of our needs than we were previously
using. The requests below concern the remaining seams, rather than asking you
to reproduce our application framework.

## 1. Extend the ergonomics around detached stores

This would remove the most routine adapter code for us.

Our broker creates a future while briefly borrowing a store, then lets that
future complete independently. One read can wait for streaming events while
other reads and writes continue. DetachedReader/DetachedWriter fit this model;
the borrowing AsyncReader/AsyncWriter traits do not substitute for it.

In 0.2.0, Serde's AsyncTypedReader/Writer helpers extend only the borrowing
traits. Detached forwarding exists for references, boxes and Shared, but the
ReadOnly, Rooted, Cascade and Masked combinators have no detached counterparts.

Could you provide detached typed read/write extensions and detached-capable
combinators? Ideally:

- The returned future is Send + 'static and releases the store borrow before
  it is awaited.
- Typed reads specify raw-record/codec behavior and preserve validation errors.
- Rooted documents result-path rebasing and rejects escaping result paths.
- ReadOnly rejects a write before constructing an effectful underlying operation.

A compile test that starts two reads before awaiting either, and a parked-read
test showing an unrelated write can finish, would make this contract concrete.

## 2. Preserve useful Serde diagnostics

The current `Error::custom` implementation discards the diagnostic text and
returns TypeMismatch. For example:

```rust
#[derive(Debug, serde::Deserialize)]
enum Mode { Streaming }
let value = structfs_serde_store::json_to_value(serde_json::json!("Misspelled"));
println!("{}", structfs_serde_store::from_value::<Mode>(value).unwrap_err());
```

With published 0.2.0 this reports TypeMismatch, losing the unknown variant and
expected alternatives that Serde supplied. That makes malformed requests and
old persisted records much harder to diagnose.

Please retain bounded diagnostic text alongside the structured category.
Nested field/index context would help too. We are happy with an explicit size
limit; we need enough information to identify and repair the offending value.

## 3. Reject malformed assembly configuration

These otherwise valid assemblies currently parse successfully:

```yaml
assembly: example
blocks: {a: 'embedded:a'}
public: a
config: false
```

The same happens with `config: {ghost: {}}`, even though there is no `ghost`
block. Ox adds validation around AssemblyDef to catch these mistakes at startup.

Please reject wrong types in standard sections and references to unknown
blocks, while explicitly identifying any extension fields that may be ignored.
We have a regression demonstrating upstream acceptance and downstream rejection
of both examples.

## 4. Make platform features easier to compose

Adding the HTTP/handles dependency to a crate shared with our browser build
enabled Tokio's `rt-multi-thread`, which fails on wasm32-unknown-unknown. We
worked around this by moving native dependencies behind target-specific Cargo
sections.

Could portable types and cancellation primitives be available without native
executor features? A documented target/feature matrix would also help. A
portable guest SDK is useful, but it doesn't by itself tell us which supporting
crates can appear in browser-shared code.

## 5. Support explicit compatibility choices for paths and patterns

Two small additions would let us retire remaining compatibility helpers:

- **Path component-array Serde adapter.** Our existing records contain
  `["settings", "accounts"]`; upstream Path now serializes as
  `"settings/accounts"`. An opt-in `serde(with = ...)` adapter for Path and
  Option<Path>, retaining component validation, would preserve existing formats
  without changing your default. We've consolidated our two copies into one.
- **Minimum middle length for suffix patterns.** Upstream
  `prefix_suffix(accounts, provider)` matches `accounts/provider`. Our
  subscriptions need at least one middle component to select account instances.
  An option for zero versus one-or-more, with a documented Serde representation,
  would let us replace our remaining pattern type without broadening watches.

Neither difference is inherently a bug. We need to choose the policy explicitly
and preserve records already on disk.

## 6. Publish a complete embedding lifecycle example

We successfully embedded Featherweight, but coordinating the lifecycle remains
substantial host code: reuse a prepared module, reserve capacity, expose broker
imports, observe terminal errors, cancel, shut down, and retain ownership and
capacity until cleanup actually joins.

The example we'd most like is an HTTP request cancelled while an external
broker is still allocating a handle. The broker may accept the write and return
its result after cancellation. Cleanup must receive that result and release the
resource even if the HTTP caller has disappeared.

OwnerHandle::open provides the needed ownership mechanism. Our old adapter's
reply timeout could discard an accepted result before ownership saw it; that
was our integration problem. We fixed it and added tests for late replies,
aliased handle paths, actual producer termination and client disconnects.

An example should also show nonzero guest exit, an incomplete cleanup report,
and which executor must outlive engine ticking and retained cleanup. A small
embedding helper would be welcome if it preserves these explicit decisions.

## 7. Enforce the path macro's documented expression type

We found a release-mode discrepancy while testing the macro migration:

```rust
use structfs_core_store::{Path, path};
struct PretendComponent;
impl PretendComponent {
    fn validated_str(&self) -> &str { "bad-name" }
}
let p = path!("safe", PretendComponent);
assert!(Path::parse(&p.to_string()).is_err());
```

This compiles, and the assertion passes in release mode. The macro calls a
method named `validated_str` without requiring PathComponent; its generated
constructor revalidates only in debug builds. Please constrain expression
arguments to the documented type and add a compile-fail test for this case.
An explicitly typed borrow could preserve reuse of the same component. Our
current callers use validated components, so we've retained the upstream macro.

## Documentation follow-ups

A migration table would help distinguish intentional changes from integration
mistakes: Path iteration, combinator accessors/matching, fallible JSON conversion,
strict numeric conversion, empty-container behavior, and the two async trait
families. In particular, typed integer Value → f64 fails where JSON integer
syntax still decodes as f64; we needed an explicit compatibility boundary for
existing usage records.

Please also document the path macro's direct-dependency requirement, or support
renamed dependencies/reexports hygienically. The release checkout we inspected
still described the release as an unpublished candidate while 0.2.0 was already
available in the registry; keeping those statuses aligned would simplify
readiness checks.

## What we still own

We retain our ledger writer, persistence formats, application subscription hooks,
provider protocols and conversation-agent runtime. We are not treating those as
missing StructFS features. State's memory-durability contract and invalidation
model are different from our disk ledger and post-write hooks; adopting it would
be a deliberate architecture change. Likewise, moving the conversation runtime
is further downstream porting work, not an established upstream blocker.

The detached ergonomics, diagnostics, assembly validation and path-macro contract
requests are our highest priorities. They would remove recurring adapter work
and make the next round of adoption easier to debug. We can turn the examples above into focused
issues or regression tests if useful.

— The Ox team
