# StructFS 0.2 cleanup audit

Audited against published 0.2.0 registry sources on September 13, 2026. Source
references below are crate-relative; local registry artifacts are authoritative.
Matching sibling source files were compared byte-for-byte before use. No local
upstream patches are required. The implementation and final validation are
tracked in the [cleanup plan](../superpowers/plans/2026-09-13-structfs-ergonomics-cleanup.md).

## Removed or consolidated

| Previous Ox implementation | Replacement | Compatibility check |
|---|---|---|
| `ox-path` proc macro and its dependencies | upstream `path!` | literal/integer/validated component tests; invalid literals and bare String/str compile-fail tests; empty literal now intentionally means root |
| ReadOnly, Cascade, Masked implementations | upstream combinators | existing wrapper tests retained; Masked has no production callers |
| Completion cancellation token | upstream CancelToken | cancellation and disconnect regressions |
| Gateway bespoke Wasm host and assembly parser | Featherweight SDK/runtime/AssemblyDef | codec, capability denial, aliases, failure and lifecycle parity tests |
| Codec JobStore and its shared map | Shared<MemoryStore> | existing codec parity suite; only job/result leaves are used |
| Broker sorted-vector prefix lookup | PathTrie | first duplicate registration still wins; exact unmount retains descendants; empty branches pruned after transient mounts |
| Duplicate Ox/Horns Path Serde implementation and component-array Value encoders | one Horns compatibility adapter and encoder | existing component-array shape, optional paths and validation preserved |
| Broker private detached async traits | upstream DetachedReader/DetachedWriter | parked-read/independent-write regression passes after dropping the original client; spawned servers retain explicit 'static bounds |

## Retained for a concrete reason

| Helper or subsystem | Why a direct replacement is unsuitable now |
|---|---|
| `LocalConfig` | Flat configuration insertion permits overlaps that fail when projected; upstream MemoryStore rejects writes through scalar ancestors. Empty maps disappear from LocalConfig's leaf-only representation but remain present upstream. A drop-in alias changes behavior. |
| Shared Path Serde adapter | Existing records contain component arrays; upstream Path Serde uses a string. Removing the adapter requires a schema migration or an upstream opt-in adapter. |
| Horns subscription PathPattern | Requires at least one middle component and has an existing serialized enum shape. Upstream suffix patterns allow an empty middle and do not supply this wire adapter. |
| Subscription dispatcher and writer capability | Ox runs hooks after writes, bounds cascades, and gives handlers a cloneable `&self` writer. StructFS state observation reports committed invalidations; it is not this hook protocol. |
| Broker channels, mounting and accepted-write policy | PathTrie replaces lookup, not actor scheduling. Accepted writes must survive cancellation; abandoned reads can be dropped. Service ownership is used at effectful gateway boundaries. |
| SyncClientAdapter | Existing implementation also supports calls inside a multi-thread Tokio runtime via block_in_place. Upstream SyncBridge explicitly requires a non-runtime worker; direct substitution would panic at such call sites. |
| `ox-context::Namespace` | First-component-only dispatch, missing reads returning None, and rebased write-result paths are its current contract. Upstream OverlayStore/service Router have broader routing/policy APIs; adopting them is a namespace contract change. |
| StoreBacking, JSONL/TOML/JSON adapters and ledger writer | Application persistence formats and commit ordering. StructFS State explicitly promises memory durability; adopting it would not replace disk persistence. |
| Remote wire v1 | Committed canonical fixtures and the original signed integer range remain a compatibility contract. Using a new value codec is a protocol version change. |
| Conversation agent runtime | Still executes through ox-runtime/ox-executor. Porting tools, event effects, approvals, cancellation and resume behavior is a separate runtime migration, not a utility alias. No new upstream blocker is established by this audit. |

## Reproduced boundaries

`cargo run --manifest-path local/structfs-migration-probe/Cargo.toml --offline`
records the following in `local/structfs-ergonomics-probe.log`:

- Empty map written at `settings`: LocalConfig reads None; MemoryStore reads
  an empty map.
- A scalar at `settings/leaf` plus a descendant: LocalConfig reports malformed
  projection on read; MemoryStore rejects the descendant write.
- Upstream suffix pattern `(accounts, provider)` matches `accounts/provider`.
- Upstream Path JSON is `"settings/accounts"`, not a component array.
- Unknown enum Serde diagnostics collapse to TypeMismatch; an integer Value
  fails typed f64 conversion even though equivalent JSON integer syntax succeeds.

A separate release-mode probe (`local/structfs-macro-contract.log`) also confirms
that the upstream macro accepts an unrelated type with a `validated_str` method,
and can construct a path the ordinary parser rejects. Current Ox callers use
validated components; SF-011 and the letter request explicit type enforcement
and a compile-fail regression upstream.

These differences are not all upstream bugs. The maintainer letter separates
ergonomic requests from application policy and intentional compatibility work.
