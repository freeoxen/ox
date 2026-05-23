# Real lint for silent-parse-fallback (replace grep gate)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the grep-based `"no silent parse fallback"` gate in
`scripts/quality_gates.sh` with a real AST-aware lint that catches
multi-line patterns and variant shapes (`.ok().flatten().unwrap_or(...)`,
`if let Ok(x) = ... { x } else { default }`, `match expr { Ok(x) => x, _ => default }`).

**Why:** The grep gate only catches single-line `<parse|extract|from_*>(...).unwrap_or(_default|_else)?(...)`.
The audit's original bug was multi-line:

```rust
serde_json::from_value(tc.input.get("refs").cloned().unwrap_or_default())
    .unwrap_or_default();
```

The grep would miss this exact shape on a future regression. A real
lint with AST awareness closes the gap.

**Background:** The silent-unwrap audit (`docs/superpowers/specs/2026-05-22-silent-unwrap-audit.md`)
closed 6 sites and pinned the pattern with a grep gate. Six existing
sites carry `// allow(silent_parse_fallback): <reason>` justifications.
The grep is good enough for the common single-line case; this plan
upgrades it to handle the cases the grep misses.

---

## Approach options

### Option A — Custom clippy lint via `dylint`

`dylint` (https://github.com/trailofbits/dylint) lets us write a
clippy-style lint as a separate crate, load it dynamically, and run
it via `cargo dylint`. The CI gate becomes:

```bash
gate "no silent parse fallback (lint)" cargo dylint --lib silent_parse_fallback
```

The lint logic walks the HIR/MIR and detects:
- `MethodCall(unwrap_or_default | unwrap_or | unwrap_or_else, receiver)` where `receiver` is
  - `MethodCall(extract | from_value | from_str, _)`
  - `MethodCall(parse, _)` on a `&str` or `String`
  - Chains containing any of the above (e.g. `.ok().flatten()` followed by `.unwrap_or(_)`)
- An `if let Ok(x) = expr { x } else { default }` shape where `expr` is parse-typed.
- A `match expr { Ok(x) => x, _ => default }` shape, same.

Suppression: the audit's `// allow(silent_parse_fallback): <reason>`
comment on or above the call.

**Cost:** ~half-day to write the lint + ~half-day to wire dylint into
CI. Total ~1 day.

**Maintenance:** dylint pins to a specific nightly toolchain; bumps
require updating the lint's toolchain file. Manageable but real.

### Option B — Custom `cargo-spellcheck`-style external tool

Write a small Rust binary that uses `syn` to parse each `.rs` file
and walks the AST to find the same patterns. Run from
`scripts/quality_gates.sh`.

```rust
// crates/no-silent-fallback/src/main.rs
fn main() {
    let violations = walk_workspace_for_violations();
    if !violations.is_empty() {
        for v in &violations {
            eprintln!("{}:{}: silent parse fallback — {}", v.path, v.line, v.detail);
        }
        std::process::exit(1);
    }
}
```

**Pros:** No dylint dependency. Stable toolchain. Lives in the
workspace. Easy to extend with new patterns.

**Cons:** Slightly more code than dylint (need to write AST-walk
boilerplate). `syn` parsing isn't full HIR; we won't see resolved
types, just syntactic shapes.

For the patterns the audit cares about, syntactic shapes are enough —
we're matching `<name with "from" or "parse" or "extract">(...)`
chained to `.unwrap_or*(...)`. Type resolution would help avoid
false positives but isn't strictly required.

**Cost:** ~half-day end-to-end. No CI infrastructure changes needed.

### Option C — `ast-grep` config

[ast-grep](https://ast-grep.github.io) is a CLI tool for AST-aware
search-and-replace. It already supports Rust grammar. Configure a
ruleset and run it from the quality-gates script:

```yaml
# ast-grep-rules/silent_parse_fallback.yml
id: silent-parse-fallback
message: silent default on parse result
rule:
  pattern: $RECEIVER.unwrap_or_default()
  inside:
    has:
      pattern: $X.from_str($_)
```

**Pros:** No Rust code to maintain. Pattern syntax is declarative.

**Cons:** Adds an external CLI dependency to the dev environment.
Pattern expressiveness is limited (more than grep, less than full
syn AST).

**Cost:** ~couple hours including learning curve.

---

## Recommendation

**Option B** (syn-based external tool). Best cost/benefit:

- Lives in the workspace (no dylint nightly pinning, no external CLI).
- Stable Rust syntax via `syn` — no toolchain bumps needed.
- ~half-day cost, comparable to dylint.
- Extensible: future patterns ("don't silently drop in scope X") fit
  the same harness.

The crate name `no-silent-fallback` keeps it discoverable. Mount
into the workspace as a binary target. Gate calls `cargo run --release --bin no-silent-fallback`.

## Tasks

- [ ] Add `crates/no-silent-fallback` workspace member with `[bin]` target.
- [ ] Walk the workspace's `.rs` files (skip `target/`, `tests/`,
      and anything in a `#[cfg(test)] mod tests` block).
- [ ] For each file, parse via `syn::parse_file`, walk the AST,
      detect the four problematic shapes:
    - `<parse-chain>.unwrap_or_default()`
    - `<parse-chain>.unwrap_or(<expr>)`
    - `<parse-chain>.unwrap_or_else(<closure>)`
    - `match <parse-chain> { Ok(x) => x, _ => <default> }`
- [ ] Respect `// allow(silent_parse_fallback): <reason>` comments
      on the same line or the line immediately above.
- [ ] Add to `scripts/quality_gates.sh` as a new gate, replacing
      the grep-based gate.
- [ ] Verify the existing grandfathered sites still pass with
      explicit allows; verify a synthetic violation fails.
- [ ] Update `docs/superpowers/specs/2026-05-22-silent-unwrap-audit.md`
      to point at the new lint.

## When to do it

When the grep gate's false negatives bite — typically the second time
a silent-parse-fallback regression slips past code review and the
grep. Until then, the grep is paying for itself.
