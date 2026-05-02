# Settings redesign — S-tier closure

> **For agentic workers:** REQUIRED SUB-SKILL: `superpowers:executing-plans`. Each task uses checkbox (`- [ ]`) syntax for tracking. This is a sharpening pass on already-shipped work, not a redesign — most tasks are mechanical or doc-only.

**Goal:** Close the five gaps between the shipped settings-redesign implementation and S-tier — three doc reconciliations, one small refactor (`AscendRule`), one dispatcher fix (dedup-by-id), one new test suite (KeyChord round-trip).

**Background:** The settings redesign (`docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` + `docs/superpowers/plans/2026-04-27-settings-screen-redesign.md`) shipped through Phase R + S2 polish. A code-grounded review (2026-05-02) graded it A and identified five surgical moves to reach S. This plan executes those moves.

**Spec:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`. Several edits in Phase A reshape it to match the implementation; do not edit it independently of this plan during execution.

**Plan organization.** Phase A reconciles the spec with the shipped code (doc-only, lands first to unblock everything else). Phase B promotes `AscendRule` to three variants and removes the `NavAscend` fallback chain. Phase C fixes the subscription overlap-firing footgun. Phase D adds the property test that would have caught the Shift+Tab class of bugs. Phase E adds one paragraph documenting the dispatcher fast-path as a spec property. Phases B–E are independent and can land in any order after A.

**Plan style.** File paths, function signatures, the test that goes first (TDD where applicable), the commit message. Code samples are illustrative.

**Estimated total effort:** 3–4 hours focused work.

---

## Phase A — Spec reconciliation (doc-only)

The spec has drifted from the shipped code in five named places. None of these touch implementation; all are corrections to the spec.

### Task A1: Fix the completion path naming

**File:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

The spec advertises `config/completions/{role_name}` (§5.3, §6.1). The implementation uses `config/gate/completions/{role_name}` everywhere — kernel (`run.rs:710, 760, 1796, 1827`), commands (`account_model.rs:337`), bootstrap, snapshot. The shorter path does not exist anywhere in the code.

- [ ] **Step 1:** §5.3 first paragraph after the `CompletionRole` struct: change `config/completions/{role_name}` → `config/gate/completions/{role_name}`. Update the prose to match: "Stored at `config/gate/completions/{role_name}`. Day-one role: `primary`."
- [ ] **Step 2:** §5.3 prose about `Compose`: leave as-is (no path mentioned).
- [ ] **Step 3:** §6.1 day-one entries table: the `models` row's badge `PrimaryReference` description (in §5.6) currently reads "from `config/completions/primary`" — change to `config/gate/completions/primary`.
- [ ] **Step 4:** §5.6 `BadgeSource` enum doc comment for `PrimaryReference`: same swap.
- [ ] **Step 5:** §6.6 Models page binding table — the `models.set_primary` row points at `config/completions/primary ←`; change to `config/gate/completions/primary ←`.
- [ ] **Step 6:** §5.9 "What's removed" table — the `gate/defaults/*` rows point at `config/completions/primary.account` and `.model` as replacements; change to `config/gate/completions/primary.account` and `.model_id` (note: `.model` should also be `.model_id` to match the `CompletionRole` field name).
- [ ] **Step 7:** Grep the spec one final time for `config/completions` (with no `gate/` prefix) and replace each remaining instance.
- [ ] **Step 8:** Commit `docs(spec): reconcile completion path with implementation (config/gate/completions)`.

### Task A2: Fix the type-location table

**File:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

Per commit `2699287`, `ModelInfo` and `CompletionRole` landed in `ox-types`, not `ox-gate`. The cycle-break is correct (kernel needs them, kernel can't depend on `ox-gate`); the spec's table was never updated.

- [ ] **Step 1:** §5.10 row `ox-gate`: remove `ModelInfo`, `ModelInfoSource`, `CompletionRole` from the type list. Keep `ApiKey`, `AccountTestStatus`, `CatalogRefreshStatus`, `KnownFamilyEntry`, `known_family_metadata()`, settings subscription impls, transport.
- [ ] **Step 2:** §5.10 row `ox-types`: add `ModelInfo`, `ModelInfoSource`, `CompletionRole` to the type list. Order alphabetically with the existing entries.
- [ ] **Step 3:** Add a one-line note below the table: "_`ModelInfo` and `CompletionRole` live in `ox-types` (not `ox-gate`) so the kernel can read them without a `kernel → gate` dependency cycle._"
- [ ] **Step 4:** Commit `docs(spec): correct type-location table (ModelInfo, CompletionRole live in ox-types)`.

### Task A3: Document SubCtx::snapshot as a live reader, not a pinned snapshot

**File:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

§3.3 currently says "snapshot pinned at post-write state." The implementation (`ox-broker/src/subscription.rs:67-78`) explicitly rejects this — the broker has no global version to pin against, so successive reads inside a handler can observe writes that landed after the trigger. This is a real semantic deviation handlers must reason about.

- [ ] **Step 1:** §3.3 runtime-contract item 2 currently reads "calls each one's `handle` with a snapshot pinned at the post-write state." Replace the trailing clause with: "calls each one's `handle` with a *live* broker reader (not a pinned snapshot — the broker has no global version to pin against). Successive reads inside a handler may observe writes that landed after the trigger; handlers reading multiple paths and reasoning about cross-path consistency must coordinate themselves (e.g. read everything they need into local variables before any await)."
- [ ] **Step 2:** §3.3 doc comment on the `Subscription` trait code block (the `/// Called synchronously after the watched write commits.` comment): adjust the second sentence to mention the live-reader semantics.
- [ ] **Step 3:** §9.7 "Why subscriptions as a protocol, not a StructFS primitive?" paragraph — add one sentence at the end: "v1 also doesn't model snapshot pinning: handlers see a live reader. Strict pinning would require either MVCC in the substrate or a per-handler clone; both are v2 design questions."
- [ ] **Step 4:** Commit `docs(spec): SubCtx::snapshot is a live reader, not a pinned snapshot`.

### Task A4: Document the multi-threaded tokio runtime requirement

**File:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

The dispatcher's `SnapshotReader` impl (`ox-broker/src/lib.rs:118-124, :171-172`) bridges sync `Reader::read` to async broker reads via `tokio::task::block_in_place`, which panics on a `current_thread` runtime. Every settings test uses `flavor = "multi_thread"`. This is a real architectural commitment the spec is silent on.

- [ ] **Step 1:** §3.3 add a new sub-paragraph after the runtime-contract list, before the "Subscriptions subsume" list:

  > **Runtime requirement.** The dispatcher's production `SnapshotReader` bridges sync `Reader::read` calls to async broker reads via `tokio::task::block_in_place`, which requires a multi-threaded tokio runtime. Callers on a `current_thread` runtime will panic on the first triggered subscription. The fast-path that skips snapshot reads when no subscription matches keeps `current_thread` callers working in the no-listener case (subscriptions only kick in when a registered listener actually exists). v1 ships multi-threaded; a future single-threaded variant would need an async `Reader` trait or a different bridging strategy.

- [ ] **Step 2:** Commit `docs(spec): note multi-threaded tokio runtime requirement`.

### Task A5: Switch every Reader signature to `&mut dyn Reader`

**File:** `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

The implementation takes `&'a mut dyn Reader` everywhere because `Reader::read` is `&mut self`. Every renderer/command/dispatch/SubCtx site has a 5-line documentation block. The spec still quotes the immutable form.

- [ ] **Step 1:** Grep `&'a dyn Reader` and `&dyn Reader` in the spec; for each occurrence in a code block (Renderer, Command, RenderCtx, CommandCtx, SubCtx, dispatch signature, the "concrete event-loop shape" snippet), change to `&'a mut dyn Reader` / `&mut dyn Reader` matching the actual implementation.
- [ ] **Step 2:** At the first occurrence (likely §3.2's "Renderers consume the data tree…" paragraph or §4.3's `Renderer` trait code block), add a footnote-style paragraph:

  > Reader signatures are `&mut dyn Reader` throughout. `Reader::read` is `&mut self` because production Reader implementations (`LiveReader`, `LocalConfig`) hold lazy decode caches — the mutation is internal to the Reader, not to observable application state. Renderers and commands remain pure with respect to the namespace.

- [ ] **Step 3:** Verify §3.2's pseudo-event-loop snippet uses `&mut snap` for the `dispatch(...)` call.
- [ ] **Step 4:** Commit `docs(spec): align Reader signatures with implementation (&mut dyn Reader)`.

---

## Phase B — `AscendRule::Fallback(Path)` variant

The spec gap that's currently papered over by `NavAscend`'s three-step fallback chain (`crates/ox-cli/src/settings/commands/navigation.rs:152-172`). Top-level pages (`settings/accounts`, `settings/models`) need to ascend to `settings/index`, but `AscendRule::NearestRegistered` walks strict ancestors only and `AscendRule::ExitScreen` exits the screen. The third behavior belongs in the renderer, not in the navigation command's body.

### Task B1: Add the variant

**Files:**
- Modify: `crates/ox-cli/src/settings/registry.rs`

- [ ] **Step 1:** Test first (in the `#[cfg(test)]` block). Add `ascend_fallback_returns_named_target`:

  ```rust
  #[test]
  fn ascend_fallback_returns_named_target() {
      let mut reg = RendererRegistry::new();
      reg.register(oxpath!("settings", "index"), fake(AscendRule::ExitScreen));
      reg.register(
          oxpath!("settings", "accounts"),
          fake_with_fallback(oxpath!("settings", "index")),
      );
      assert_eq!(
          reg.ascend(&oxpath!("settings", "accounts")),
          Some(oxpath!("settings", "index")),
      );
  }
  ```

  And `ascend_fallback_target_must_be_registered_or_returns_none` — covers a guard against typos that would silently break navigation.

- [ ] **Step 2:** Extend the enum:

  ```rust
  pub enum AscendRule {
      NearestRegistered,
      /// Top-level page within a screen: ascend to the named cursor (typically
      /// the screen's index page). The named target must be a registered
      /// cursor; if it isn't, the registry falls through to None and the
      /// dispatcher signals `_request_exit` (same as `ExitScreen`).
      Fallback(Path),
      ExitScreen,
  }
  ```

- [ ] **Step 3:** Extend `RendererRegistry::ascend`:

  ```rust
  pub fn ascend(&self, cursor: &Path) -> Option<Path> {
      let renderer = self.specs.get(cursor)?;
      match renderer.ascend_to() {
          AscendRule::ExitScreen => None,
          AscendRule::NearestRegistered => self.nearest_registered_parent(cursor),
          AscendRule::Fallback(target) => {
              if self.specs.contains_key(&target) { Some(target) } else { None }
          }
      }
  }
  ```

  Note `ascend_to` now returns `AscendRule` by value, not `Copy`. Update the trait signature: `fn ascend_to(&self) -> AscendRule;` (no change needed — it already returned by value). Internal implementations may hold a `Path` field they clone.

- [ ] **Step 4:** Test fixture helper `fake_with_fallback(target: Path) -> Box<dyn Renderer>` — analogous to `fake(rule)` but holding the Fallback target.
- [ ] **Step 5:** Run `cargo test -p ox-cli settings::registry::tests` — all PASS.
- [ ] **Step 6:** Commit `feat(cli): AscendRule::Fallback variant for top-level pages`.

### Task B2: Switch `AccountsListRenderer` and `ModelsListRenderer` to `Fallback`

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/accounts_list.rs`
- Modify: `crates/ox-cli/src/settings/renderers/models_list.rs`

- [ ] **Step 1:** Each renderer's `ascend_to` currently returns `NearestRegistered`. Change to:

  ```rust
  fn ascend_to(&self) -> AscendRule {
      AscendRule::Fallback(oxpath!("settings", "index"))
  }
  ```

- [ ] **Step 2:** Update each renderer's existing tests (if any assert `ascend_to`) to expect `Fallback(oxpath!("settings", "index"))`.
- [ ] **Step 3:** Run `cargo test -p ox-cli settings::renderers::accounts_list settings::renderers::models_list` — all PASS.
- [ ] **Step 4:** Commit `feat(cli): top-level renderers declare Fallback to settings/index`.

### Task B3: Restore `NavAscend::run` to its original two-line shape

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/navigation.rs`

The three-step fallback chain (lines 152-172) becomes a regression: with `Fallback` variants on the top-level renderers, `registry.ascend(cursor)` returns `Some(parent)` for accounts/models and `None` only when at the index. The command body collapses.

- [ ] **Step 1:** Replace the `ascend` function body with:

  ```rust
  fn ascend(
      data: &mut dyn Reader,
      ctx: &crate::settings::command_registry::CommandCtx<'_>,
  ) -> Vec<Write> {
      let cursor = match read_path(data, &oxpath!("ui", "settings", "cursor")) {
          Some(c) => c,
          None => return Vec::new(),
      };
      match ctx.registry.ascend(&cursor) {
          Some(parent) => vec![Write {
              path: oxpath!("ui", "settings", "cursor"),
              record: Record::parsed(path_to_value(&parent)),
          }],
          None => vec![Write {
              path: oxpath!("ui", "settings", "_request_exit"),
              record: Record::parsed(Value::Bool(true)),
          }],
      }
  }
  ```

- [ ] **Step 2:** The two existing regression tests (`ascend_top_level_page_falls_back_to_settings_index` and `ascend_at_exit_screen_writes_request_exit`) should still pass — they exercise the same observable behavior. Verify before commit.
- [ ] **Step 3:** Run `cargo test -p ox-cli settings::commands::navigation::tests` — all PASS.
- [ ] **Step 4:** Run the e2e harness: `cargo test -p ox-cli --test settings_e2e` — all PASS.
- [ ] **Step 5:** Commit `refactor(cli): NavAscend uses AscendRule::Fallback; remove fallback chain`.

### Task B4: Update the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

- [ ] **Step 1:** §4.1 the `AscendRule` enum code block: add the `Fallback(Path)` variant with the comment "Top-level page within a screen; ascend to the named cursor (typically the screen's index page). The named target must be a registered cursor."
- [ ] **Step 2:** §4.1 the prose after the enum: change the last sentence from "`settings/index` uses `ExitScreen`. Everything else uses `NearestRegistered`." to "`settings/index` uses `ExitScreen`. Top-level pages (`settings/accounts`, `settings/models`) use `Fallback(settings/index)`. Detail pages and overlays use `NearestRegistered`."
- [ ] **Step 3:** §9 "Why not?" — add a short rationale at the end:

  > **§9.12 Why three AscendRule variants instead of two?** v0 of this design had two — `NearestRegistered` (strict-ancestor walk) and `ExitScreen`. Top-level pages fell into a gap: their parent in the display tree is the screen's index, but the index isn't an ancestor of `settings/accounts` (they're siblings under `settings/`). The `Fallback(Path)` variant lets the renderer declare its ascent target explicitly, keeping the routing decision in the renderer where it belongs rather than in `NavAscend`'s body.

- [ ] **Step 4:** Commit `docs(spec): document AscendRule::Fallback variant and rationale`.

---

## Phase C — Subscription dedup

`crates/ox-broker/src/subscription.rs:108-111` documents that registering `[Prefix(p), PrefixSuffix{p, suffix}]` causes the subscription to fire twice on overlapping paths. This is a footgun — the natural intuition is "patterns are a union" but the dispatcher treats them as independent fires.

### Task C1: Dedup-by-id in the dispatcher

**Files:**
- Modify: `crates/ox-broker/src/dispatching_store.rs`
- Modify: `crates/ox-broker/src/subscription.rs` (doc-only)

- [ ] **Step 1:** Test first. Add to `dispatching_store.rs::tests`:

  ```rust
  #[tokio::test]
  async fn overlapping_patterns_invoke_handler_once() {
      // A subscription with two patterns that both match the same path.
      // The handler should fire exactly once, not once per matching pattern.
      let fire_count = Arc::new(Mutex::new(0u32));
      let fire2 = fire_count.clone();
      let mut reg = SubscriptionRegistry::new();
      reg.register(closure_sub(
          "multi-pattern",
          vec![
              PathPattern::Prefix(oxpath!("p")),
              PathPattern::PrefixSuffix {
                  prefix: oxpath!("p"),
                  suffix: oxpath!("suffix"),
              },
          ],
          Box::new(move |_c, _w, _s| {
              *fire2.lock().unwrap() += 1;
              vec![]
          }),
      ));
      let (disp, _data, _spawn) = build(reg, 64);

      // Path matches BOTH patterns.
      disp.write(&oxpath!("p", "x", "suffix"), Record::parsed(Value::Integer(1)))
          .await
          .unwrap();

      assert_eq!(*fire_count.lock().unwrap(), 1, "handler must fire once per write, not per matching pattern");
  }
  ```

- [ ] **Step 2:** In `write_at_depth`, after `let matched = me.subs.read()...matching(&path);`, dedup by `Arc::ptr_eq` (preserves order, removes duplicates):

  ```rust
  let mut matched = me.subs.read().expect("registry lock poisoned").matching(&path);
  matched.dedup_by(|a, b| Arc::ptr_eq(a, b));
  ```

  `Vec::dedup_by` only removes consecutive duplicates, but since `matching` returns subs in registration order and a single subscription's multiple patterns are inserted contiguously, this is correct. (If the registry's iteration order ever changes such that a sub's patterns are interleaved with others', this dedup needs to switch to a HashSet by pointer — add a comment noting the dependency.)

- [ ] **Step 3:** Update the existing `unique_subscription_returned_when_pattern_overlaps` test in `subscription.rs::tests`. That test currently asserts the *registry* returns the subscription twice (which is still correct behavior at the registry level). Add a comment clarifying the dispatcher dedups; the registry doesn't.
- [ ] **Step 4:** Update the doc on `Subscription::watches` in `subscription.rs`: change the existing paragraph "If a single write matches more than one of this subscription's patterns, `handle` fires *once per matching pattern* — the dispatcher does not deduplicate by id. Authors who want at-most-once semantics should ensure their pattern set is disjoint…" to:

  > If a single write matches more than one of this subscription's patterns, the dispatcher invokes `handle` *exactly once per write* (dedup-by-id). Authors can think of `watches()` as a union of paths the subscription cares about; overlap is safe.

- [ ] **Step 5:** Run `cargo test -p ox-broker subscription dispatching_store` — all PASS, including the new test.
- [ ] **Step 6:** Commit `fix(broker): dispatcher dedups subscriptions by id; overlapping patterns fire once`.

### Task C2: Update the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

- [ ] **Step 1:** §3.3 runtime-contract item 2: extend the sentence about subscription invocation. Currently reads "calls each one's `handle` with a snapshot pinned…" After Phase A this reads "…with a *live* broker reader…". Add at the end of the same item:

  > Each matching subscription is invoked exactly once per triggering write; if a subscription's `watches()` lists multiple patterns that overlap on the triggering path, the dispatcher dedups so the handler doesn't fire multiple times. Authors can think of `watches()` as a union.

- [ ] **Step 2:** Commit `docs(spec): subscription handler fires once per write (dedup-by-id)`.

---

## Phase D — KeyChord round-trip property tests

The `Shift+Tab` bug (`53d3da2`) was a real correctness gap that bit because three places have to agree:

- `key_encode::encode_key(KeyEvent) → String` — wire form
- `dispatch::parse_key_str(String) → KeyChord` — parser
- `bindings.rs::register(BindingEntry { key: KeyChord, ... })` — what bindings register

If the encoder and parser disagree, every binding using the affected chord silently misses. A property test over the canonical chord set would have caught it.

### Task D1: Define the canonical chord set

**Files:**
- Create: `crates/ox-cli/src/key_chord_canonical.rs` (or extend an existing key-chord helper module — check `crates/ox-cli/src/dispatch.rs` for the right home)
- Modify: `crates/ox-cli/src/lib.rs` (declare module if new)

- [ ] **Step 1:** Define a function `pub fn canonical_chords() -> Vec<KeyChord>` that produces every `KeyChord` we care about supporting in bindings. Cover:
  - Every `KeyCodeRepr` variant: `Char` (a-z, A-Z, 0-9, common symbols), `Enter`, `Esc`, `Tab`, `BackTab`, `Backspace`, `Delete`, `Up`, `Down`, `Left`, `Right`, `PageUp`, `PageDown`, `Home`, `End`, `Insert`, `F(1..=12)`.
  - Every modifier subset: no modifiers, `ctrl`, `shift`, `alt`, `super_`, plus the meaningful combinations (`ctrl+shift`, `ctrl+alt`).
  - Skip nonsense chords (e.g. `shift+Up` is fine; `ctrl+ctrl` is impossible to represent).
- [ ] **Step 2:** Cap at ~200 chords. The set is hand-curated, not exhaustive — alphabet × modifier-subsets × keycodes is too many. Cover what's actually used in bindings plus a representative sample of unused-but-valid chords for future-proofing.
- [ ] **Step 3:** Sanity test in the same module: assert `canonical_chords()` returns at least 100 entries and contains specific anchors (`ctrl+s`, `Shift+Tab`, `F1`, plain `Esc`, plain `j`).
- [ ] **Step 4:** Commit `feat(cli): canonical KeyChord set for property testing`.

### Task D2: Encoder/parser round-trip property test

**Files:**
- Modify: `crates/ox-cli/src/dispatch.rs` (where `parse_key_str` lives) or wherever the encoder lives

- [ ] **Step 1:** Add the round-trip test:

  ```rust
  #[test]
  fn keychord_encode_parse_roundtrip() {
      use crate::key_chord_canonical::canonical_chords;
      let mut failures = Vec::new();
      for chord in canonical_chords() {
          let wire = encode_keychord_to_str(&chord);  // helper that mirrors what
                                                       // key_encode::encode_key produces
                                                       // for an equivalent KeyEvent
          match parse_key_str(&wire) {
              Some(parsed) if parsed == chord => {}
              Some(parsed) => failures.push(format!(
                  "chord {chord:?} encoded to {wire:?}, parsed back as {parsed:?} (mismatch)"
              )),
              None => failures.push(format!(
                  "chord {chord:?} encoded to {wire:?}, parser returned None"
              )),
          }
      }
      if !failures.is_empty() {
          panic!("round-trip failures:\n{}", failures.join("\n"));
      }
  }
  ```

  `encode_keychord_to_str` may not exist as a direct helper — `key_encode::encode_key` takes a `KeyEvent`. Either add the helper that constructs the equivalent `KeyEvent` and calls the existing encoder, or factor `encode_key` into a `KeyChord → String` function and a thin `KeyEvent → KeyChord` conversion.

- [ ] **Step 2:** Run the test. If it fails (likely — Shift+Tab was just one example of the class), enumerate the failures and either fix the parser or the encoder per case. Each fix gets its own commit; this commit is just the test infrastructure.
- [ ] **Step 3:** Commit `test(cli): encoder/parser round-trip property test for KeyChord`.

### Task D3: Binding-registry round-trip

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1:** Add a test that registers the day-one bindings, then for every registered `BindingEntry` synthesizes the equivalent `KeyChord` and asserts `BindingRegistry::lookup` resolves to the right `CommandId`:

  ```rust
  #[test]
  fn every_registered_binding_round_trips_through_lookup() {
      let mut reg = BindingRegistry::new();
      register(&mut reg);
      for entry in reg.entries() {  // expose a `pub fn entries(&self) -> &[BindingEntry]` if not already
          let cmd = reg.lookup(
              entry.screen,
              entry.cursor_path.as_ref().unwrap_or(&oxpath!()),
              entry.mode,
              &entry.key,
          );
          assert_eq!(
              cmd, Some(&entry.command_id),
              "binding {entry:?} did not round-trip through lookup",
          );
      }
  }
  ```

  If `BindingRegistry` doesn't expose `entries()`, add it (returns `&[BindingEntry]` or an iterator).

- [ ] **Step 2:** Run the test. Resolve any failures (binding-specificity sort issues, lookup edge cases).
- [ ] **Step 3:** Commit `test(cli): every registered binding resolves to its own command via lookup`.

### Task D4: Encoder → parser → lookup end-to-end

**Files:**
- Modify: `crates/ox-cli/src/dispatch.rs`

- [ ] **Step 1:** Add an end-to-end round-trip that closes the loop: for every day-one binding, take its `KeyChord`, encode it to wire format, parse it back, dispatch through `dispatch_settings_key`, assert the right command ran. Catches the full triangle.

  ```rust
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn day_one_bindings_round_trip_through_full_dispatch() {
      // For each binding (entry, target_command), simulate:
      //   encode_keychord_to_str(entry.key) → wire
      //   parse_key_str(&wire) → chord
      //   dispatch_settings_key(snap, screen, cursor, mode, &chord, ...) → writes
      //   assert writes are non-empty and the right command ran (e.g. by
      //   probing the writes' paths)
      // ...
  }
  ```

  Couple-hundred line test. Worth it — this is the test that proves the whole keystroke pipeline works.

- [ ] **Step 2:** Run; resolve failures.
- [ ] **Step 3:** Commit `test(cli): day-one bindings dispatch end-to-end via encoded keystrokes`.

---

## Phase E — Document the dispatcher fast-path as a spec property

The fast-path at `dispatching_store.rs:121-130` (skip snapshot reads when no subscription matches) is a real behavior that gives both performance (one substrate write per call in the no-listener case) and runtime compatibility (`current_thread` callers don't hit the `block_in_place` bridge in the no-listener case). The spec doesn't acknowledge it.

### Task E1: Add the fast-path paragraph to the spec

**Files:**
- Modify: `docs/superpowers/specs/2026-04-27-settings-screen-redesign.md`

- [ ] **Step 1:** §3.3 add a new sub-paragraph after the runtime-requirement paragraph (added in A4):

  > **Fast-path.** When no registered subscription matches the written path, the dispatcher skips the snapshot read and returns immediately after the substrate write. This bounds the no-listener case at one substrate write per call (no extra round-trips) and keeps `current_thread` callers functional in the no-subscription case (the `block_in_place` bridge is only entered when a handler will actually run). The fast-path is a load-bearing design property, not an incidental optimization — implementations that re-derive this dispatcher should preserve it.

- [ ] **Step 2:** Commit `docs(spec): document dispatcher fast-path as load-bearing design property`.

---

## Phase F — Verification

### Task F1: Full workspace check

- [ ] **Step 1:** `cargo test --workspace --exclude ox-wasm --exclude ox-emscripten` — all PASS.
- [ ] **Step 2:** `cargo clippy --workspace --exclude ox-wasm --exclude ox-emscripten -- -D warnings` — clean.
- [ ] **Step 3:** `cargo fmt --workspace --check` — clean.
- [ ] **Step 4:** No commit; this is verification.

### Task F2: Final spec/code reconciliation grep

- [ ] **Step 1:** From the repo root, grep for the three things that should now be consistent:
  - `rg "config/completions[^/]" docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` — should return zero hits (Task A1).
  - `rg "&'a dyn Reader|&dyn Reader" docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` — should return zero hits in code-block sections (Task A5).
  - `rg "snapshot pinned" docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` — should return zero hits (Task A3).
- [ ] **Step 2:** Check `ascend` references in spec match the three-variant model:
  - `rg "AscendRule::Fallback" docs/superpowers/specs/2026-04-27-settings-screen-redesign.md` — should return at least one hit (Task B4).
- [ ] **Step 3:** No commit; this is verification.

---

## Self-review checklist

- **Spec coverage of identified gaps:**
  - Path naming drift (`config/completions` vs `config/gate/completions`) → Task A1.
  - Type-location table drift → Task A2.
  - SubCtx::snapshot semantics → Task A3.
  - Multi-threaded runtime requirement → Task A4.
  - Reader signature drift → Task A5.
  - AscendRule shape gap → Phase B.
  - Subscription overlap-firing footgun → Phase C.
  - KeyChord round-trip bug class → Phase D.
  - Fast-path as undocumented property → Phase E.

- **Sequencing.** Phase A is doc-only and lands first to establish the spec/code baseline. Phases B/C/D/E are independent and can land in any order. Phase F verifies everything together.

- **Risk.** Phase A is zero-risk (doc-only). Phase B is mechanical refactor, low risk (existing tests catch regressions). Phase C is a one-line dedup behind a new test, low risk. Phase D is pure test addition, zero risk to runtime behavior. Phase E is doc-only.

- **Forward-compat preserved.** No type relocations, no protocol changes, no new abstractions. This plan tightens; it doesn't grow.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-02-settings-redesign-s-tier-closure.md`.**

6 phases, ~14 tasks, ~3-4 hours focused work. **Recommended: inline execution with checkpoints between phases.** Each phase is small enough that the engineer can hold the changes in working memory; subagent dispatch overhead would dominate. Phase A in particular is a single read-edit-commit pass; subagent for that would be wasted ceremony.

If the engineer wants parallelism, Phases B/C/D/E can land concurrently after A — but A first.
