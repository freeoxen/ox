# AGENTS.md

Instructions for agents (Claude Code, other LLM-driven contributors) working in this repository.

## Read this before writing a plan

The repository has an established pattern: **plan documents** live in `docs/superpowers/plans/` and are executed by sub-agents one task at a time. Plans that reference specific code (files, functions, types, call sites) are assumed to be *verified* — reviewers will trust the plan's architectural claims without re-checking each one.

**This makes un-validated claims expensive.** If a plan says "`LogStore::append` is the sole entry point for appending to `SharedLog`," reviewers won't re-read `log.rs` to confirm. If the claim is wrong, the mistake propagates into implementation work.

### The anti-pattern: "un-validated rumor"

A rumor is a confident-sounding architectural claim made from memory, inference, or a prior session's summary — not from reading current code. Symptoms:

- Phrases like "the kernel is a pure function of the log" stated as fact, with no `file:line` citation.
- Type names used without verification (`ApprovalRequest` vs `LogEntry::ApprovalRequested` — distinct types, often conflated).
- Responsibility claims ("`save_thread_state` writes the ledger") that happen to be partially true but miss the function's other responsibilities.
- Mental-model vocabulary ("agent worker," "parked coroutine," "async append") that doesn't match the code's actual shape.

Rumors feel fine to write because they're internally consistent. They fail when verified.

**Remedy:** verify before claiming. The verification is almost always faster than the debate that follows a wrong claim.

## Plan Verification Manifest (required)

Every plan touching store contracts, file formats, cross-crate seams, or data shapes must include a **Prerequisites** section where each architectural claim is:

1. Stated as a check-box item.
2. Marked `[x]` (verified) or `[ ]` (pending) with **the grep / read command that verified it** and the resulting `file:line` reference.
3. Annotated with scope if the check fails — "if this isn't true, the plan is blocked for ~N days."

Example of acceptable form:

```
- [x] **P2. `SharedLog::append` is the single in-memory append method.**
  Verified at `ox-kernel/src/log.rs:128` — `pub fn append(&self, entry: LogEntry)`.
  `LogStore::write` (line 322) dispatches here. No bypass paths found via `Grep "SharedLog" crates/`.
```

Example of unacceptable form (pre-verification):

```
- [ ] `SharedLog::append` is the single entry point.  ← no file:line, no command, no scope
```

Plans without verified prerequisites are not ready for review. If the plan author hasn't done the homework, the reviewer can't do it either.

## Don't trust session memory

You may be started with auto-memory (`MEMORY.md` files in your session's memory directory) that summarize prior conversations about this codebase. **Treat these as hints, not as source of truth.** They can be:

- Stale (the crate list drifts, new variants get added).
- Aspirational (recording what someone wanted to build, not what shipped).
- Wrong-by-inference (a past agent made a claim from memory, it got cached).

Before acting on an architectural claim from memory:

- Read the current file. Cite `file:line`.
- If the code contradicts memory, update memory; don't act on the stale claim.
- If memory says "X exists," run `Grep` for X before recommending changes that depend on X.

Your confidence level when working from memory should be lower than when working from read. This isn't optional — the last round of plan drift in this repo was traced to exactly this pattern.

## Reading order for codebase grounding

When starting work that touches the durability / state / approval paths, read these before writing anything:

1. [`docs/architecture/data-model.md`](docs/architecture/data-model.md) — where each type lives and what crosses which boundary.
2. [`docs/architecture/life-of-a-log-entry.md`](docs/architecture/life-of-a-log-entry.md) — write path for a single log entry, from creation to disk.
3. [`docs/architecture/save-and-restore.md`](docs/architecture/save-and-restore.md) — what `save_thread_state` actually does (three responsibilities).
4. The current source of any function you plan to change. Not a remembered version.

If any of these is stale, update it before using it to ground a plan.

## Commits and hygiene

- Create *new* commits; don't amend. See the tooling docs for why.
- Don't skip hooks (`--no-verify`) without the user's explicit request. If a hook fails, diagnose rather than bypass.
- Quality gates live in `./scripts/quality_gates.sh`. Run before claiming a task complete.
- Format conventions: `./scripts/fmt.sh`.

## When in doubt

Ask. Specifically, surface the architectural claim you're uncertain about *before* writing 500 lines of plan on top of it. A 30-second verification is cheaper than a 30-minute plan revision.
