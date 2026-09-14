# What would help Ox migrate further to StructFS and Featherweight

Draft for the maintainers — September 14, 2026. Not sent.

Hello,

We have upgraded Ox to the published 0.3.0 packages. The changes you made in
response to our earlier feedback have let us delete code: our component-array
Path Serde implementation, duplicate PathComponent type, extra assembly
validation pass, and hand-written broker typed conversion. We also disabled
unused native HTTP and handles features. Our gateway uses Featherweight's
prepared execution and guest SDK, and we rebuilt both packaged Wasm guests.

All 13 downstream quality gates passed, including 2,342 Rust tests, native and
browser checks, coverage, and the UI build. The improved Serde diagnostics and
stricter path macro are working for us. We also reviewed the new example for
HTTP disconnects during allocation, late replies, aliased handles and joined
cleanup. It addresses the lifecycle example we requested; we have not separately
run that upstream example, but our own gateway lifecycle tests pass.

We would like to migrate further. Below are the remaining barriers, the work
we currently own around them, and the changes that would make adoption easier.
These requests concern the 0.3.0 contracts we inspected and tested.

## 1. Prepared execution with recoverable synchronous host state

This is the largest remaining Featherweight opportunity: replacing our CLI
conversation runner.

Our runner compiles a module once and creates fresh instances for successive
turns. Host effects are synchronous. After execution, it returns the host store
and accumulated effects to the caller even when execution fails. The executor
needs those effects for bookkeeping and the backend for subsequent turns. We
also apply memory, fuel, timeout and cancellation policy, including trapping
when a guest attempts growth beyond its memory limit.

Against published 0.3.0, our probe still produces these results:

| Operation | Observed result |
|---|---|
| Run a prepared artifact synchronously | Rejected: prepared artifacts require run_async |
| Run raw module bytes synchronously | Compiles for the run; public API has no memory-cap argument and returns an exit result, not host state |
| Prepared async run with a one-page memory cap; guest ignores failed growth | Guest returns success |
| Prepared run with a 5 ms epoch interval | Rejected: prepared engine requires 10 ms |

The memory probe starts with one Wasm page and executes:

```wat
(func (export "run") (result i32)
  (drop (memory.grow (i32.const 1)))
  (i32.const 0))
```

With `CoreWasmEngine::with_limits(1, 2, 65536)`, prepared async execution returns
`Ok(0)`. It enforces the cap, but does not expose the trap-on-denied-growth policy
our runner uses. In a separate raw synchronous run, returning `memory.size`
after the growth yields `Ok(2)`.

Could you provide prepared execution over synchronous stores with host-state
recovery on success and failure, plus configurable growth-failure policy? An
official adapter over the async runner would also help if it keeps blocking host
effects off executor workers, retains accepted effects through cancellation,
and returns host state only after those effects have finished. If cleanup is
incomplete, ownership should remain explicit until it joins.

Useful acceptance cases would cover prepared-module reuse, a trap after an
accepted write, cancellation while a host operation is outstanding, independent
cancellation of simultaneous runs, and a guest that ignores failed memory growth.
Our production remote configuration already uses 10 ms epochs, so the interval
restriction alone does not block it.

We can build more channels, ownership wrappers and cancellation bridges, but
that would add integration code around a runner that already meets our needs.
Even with these APIs, migrating our guest ABI and host effects remains our work;
we are asking for the execution contracts that would make that work worthwhile.

## 2. Borrowed, allocation-free suffix matching

The new minimum-middle option expresses our subscription rule correctly. We
need `accounts/<one-or-more components>/provider` to match, while
`accounts/provider` must not match. That expressiveness request is resolved.

Two details still prevent a straightforward replacement of our matcher:

- Upstream suffix matching calls `strip_prefix` and `slice`, which construct
  owned paths by copying component vectors. Our matcher compares borrowed
  components on each dispatched write. This is a source-level allocation
  observation; we have not measured a latency regression.
- Our persisted enum uses component-array paths and a struct-shaped
  `prefix_suffix`. The upstream representation uses string paths and a separate
  minimum-middle variant. A direct alias changes existing records.

Could `PathPattern::matches` operate on borrowed components? A borrowed predicate
accepting path, prefix, suffix and minimum-middle length would also let our
legacy enum delegate without constructing an owned upstream pattern per match.

That would remove our matching implementation while allowing us to keep the
small serialization compatibility layer. An allocation-count regression for
repeated suffix matches, including empty suffixes and misses, would make the
performance contract useful to downstream users. We do not need the upstream
serialization default changed.

## 3. A standard shareable detached writer handle

The detached store traits now fit our broker's scheduling model well. Our UI
subscription tasks have a related client-handle contract that we still implement
locally: an `Arc<dyn AsyncWriter>` with a method shaped like this:

```rust
fn write(&self, path: Path, record: Record)
    -> BoxFuture<Result<Path, StoreError>>;
```

The handle is `Send + Sync`, and its returned future is `Send + 'static`.
Multiple tasks can begin independent writes through a shared reference.
`DetachedWriter` instead takes `&mut self`. That is appropriate for constructing
operations against a mutable store, but does not directly replace this erased,
shared handle interface.

Could you supply a standard handle or adapter with this contract? Any internal
lock should protect operation construction only, never remain held while a
request is parked. Please make clear when an operation is accepted and what
dropping its future does; detachment alone must not imply cancellation safety.

This would let us retire a small local trait and its plumbing. Our ordering,
post-write subscription hooks and cascade limits would remain application code.

## 4. Explicit snapshot construction that preserves Null

We would like to use MemoryStore for more temporary UI data. Our settings
snapshot currently imports `(Path, Value)` entries through `LocalConfig::set`,
which preserves Null as a stored value. Replaying those entries through
MemoryStore writes would instead delete Null leaves.

We understand that `MemoryStore::with_root` can preserve a prebuilt tree. The
remaining work is constructing that tree from flat entries and choosing what
happens when entries overlap. For example, importing `settings/example = Null`
should be distinguishable from omitting it, while importing both `a = 1` and
`a/b = 2` needs an explicit conflict policy.

A documented flat-entry import recipe or builder would help. It should preserve
present Null and empty containers, validate components, and define duplicate and
ancestor/descendant conflict behavior. Rejecting ambiguous input would be a
reasonable default. Construction can remain distinct from ordinary writes;
we are not asking you to change Null-as-deletion semantics.

This would make it easier to migrate selected scratch stores and snapshot
construction. We still need to decide which of our flat-store behaviors to
retain, and adapt subtree enumeration and renderer tests accordingly. It is
partly a downstream compatibility project, not a missing generic store.

## 5. Smaller migration difficulties and documentation requests

**Typed raw-record behavior differs between helper families.** Synchronous
`TypedReader::read_typed` decodes raw JSON, while
`DetachedTypedReader::read_typed_detached` rejects raw records through NoCodec.
We explicitly use `read_as(..., &NoCodec)` for our synchronous broker facade so
both facades behave alike. A side-by-side table for parsed, raw JSON, other raw
formats and absent records would prevent surprises. Explicit codec methods
already provide a workable choice; any alignment should preserve intentional
compatibility choices.

**The fixed macro requires migrating downstream wrapper types.** Ox still had
a distinct validated component type. The stricter macro correctly rejected it,
and reexporting yours removed that implementation. Please mention this migration
case and the constructor error-type change consumers may encounter. Hygienic
support for renamed dependencies and macro reexports would remain useful, though
the documented direct-dependency requirement is workable today.

**Release documentation lagged publication in our inspected checkout.** All
seven direct packages resolved at 0.3.0, while checkout `c8a9b15` still described
0.3 as unreleased and said no publication had occurred. Synchronizing release
status with registry availability would make downstream readiness checks easier.
This observation is tied to that checkout, not a claim about later revisions.

## Priorities and downstream responsibilities

Prepared synchronous hosting is the highest-impact request. Borrowed matching
and a shared writer handle would remove smaller, recurring pieces of integration
code. Snapshot import is useful, but needs downstream format and behavior choices
before we can replace all affected stores. The documentation clarifications would
make each of these migrations easier to assess.

We also found more cleanup we can do without waiting for you: backing our
AccountName wrapper with your validated PathComponent and replacing the kernel's
remaining manual typed-read helper. Those are our adoption tasks.

Our ledger durability, provider protocols, UI dispatch and existing remote wire
format remain application responsibilities. We are not asking StructFS state to
replace disk persistence or Featherweight to supply Ox's conversation policy.
The goal is to use more of your implementation where the contracts fit, and keep
our code focused on those application decisions.

Thank you for the 0.3 work. The earlier changes already removed code and made
failures easier to diagnose. These are the next contracts that would let us
continue that migration.

— The Ox team
