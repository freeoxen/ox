# Settings page as a Stack of Sections

**Date:** 2026-05-14
**Status:** Design — pending implementation plan
**Crates touched:** `ox-cli` (IndexRenderer, dispatch tests, snapshots)

## 1. Summary

The settings page renders as `Frame → List { items: [everything] }` — one flat homogeneous list standing in for a heterogeneous accordion (section headers, account rows, model rows, decorations, the compose form). The flatness forces two patterns:

- **Insertion-offset math.** Decorations (`+ New connection`, empty-catalog lines, manual-add affordance) live as `ListItem`s inserted at positions computed against the rows vector. After one insert, indices diverge from the rows projection — yesterday's Models decoration bug came from exactly this.
- **Compose form as a Stack sibling.** When the compose form is active, the page becomes `Frame → Stack[ Form, List ]`. The form sits ABOVE the entire list including the Accounts header, contradicting the spec's "inline at the top of the Accounts section."

This design refactors the page into a Stack of typed sections. The compose form lives INSIDE the Accounts section by construction; decoration math evaporates because each region has its own sub-List.

## 2. Goals & non-goals

### Goals

- Page is `Frame → Stack[ AccountsSection, ModelsSection ]`.
- Each section is itself a `Stack[ HeaderList, optional_middle, optional_ContentList ]`.
- Compose form sits in the Accounts section's `optional_middle` slot when active.
- `+ New connection` affordance sits in the Accounts section's `optional_middle` slot when compose is inactive AND Accounts is expanded.
- Empty-catalog decorations live in the Models section's content list as additional decoration ListItems — no inter-section index gymnastics.
- No new `View` variants. No new `ListItem` fields. Composition uses what's already there: `Stack`, `List`, `Form`.
- Selection (`ui/settings/focused`) still works. Each sub-`List` independently computes its `selected: Option<usize>`; at most one returns `Some`.
- j/k navigation unchanged — it operates on the `visible_rows` projection, not the View tree.

### Non-goals

- Scrolling. The page already doesn't scroll meaningfully; this refactor doesn't add scroll behavior. If the total content exceeds the terminal height, it's clipped, same as today.
- New section types. Today there are exactly two sections (Accounts, Models) defined by `settings/index/entries`. The refactor honors that exactly.
- Generalizing to a `View::Section` primitive. Stack-of-Lists is enough.
- Migrating the inbox/threads screens. They have their own renderers; this refactor is local to the settings IndexRenderer.

## 3. The structure

Concrete view-tree shape for the settings page:

```
Frame {
  title, title_right,
  content: Stack {
    dir: Vertical,
    children: [
      // Accounts section
      ( AccountsSection, Sizing::Min(0) ),
      // Models section
      ( ModelsSection, Sizing::Min(0) ),
    ]
  }
}

AccountsSection = Stack {
  dir: Vertical,
  children: [
    // Header
    ( List { items: [Accounts header item], selected: <if cursor matches> }, Sizing::Fixed(1) ),
    // Middle slot — compose form, affordance, or nothing
    <when compose_active>: ( Form { ... }, Sizing::Min(form_height) ),
    <when !compose_active AND accounts_expanded>: ( List { items: [affordance item] }, Sizing::Fixed(1) ),
    // Content — account rows and their expanded field rows
    <when accounts_expanded>: ( List { items: [account rows and fields], selected: ... }, Sizing::Min(0) ),
  ]
}

ModelsSection = Stack {
  dir: Vertical,
  children: [
    // Header
    ( List { items: [Models header item], selected: ... }, Sizing::Fixed(1) ),
    // Content — model rows + empty-catalog decorations interleaved
    <when models_expanded>: ( List { items: [model rows, empty-state lines, manual-add affordances], selected: ... }, Sizing::Min(0) ),
  ]
}
```

`Sizing::Fixed(1)` for header lists keeps them at one row. `Sizing::Min(0)` for content lists lets them size to their content. The page-level Stack sums section heights; if total exceeds terminal, clipping at the bottom (same as today).

## 4. Selection

`ui/settings/focused` is the cursor path. The renderer walks rows in render order to determine which section/sub-list owns the focused row, and sets `selected: Some(idx_within_that_list)` only on that sub-List. All other sub-Lists set `selected: None`.

The existing `View::List { selected: Option<usize> }` field is used per sub-List. No framework change.

## 5. j/k navigation

The `tree.next` / `tree.prev` commands operate on `visible_rows::enumerate(data)` — the flat projection — and write the next cursor path. They are independent of the View tree's section structure. Unchanged.

The renderer re-renders next frame with the new cursor; whichever sub-List contains the new focused item sets its `selected` accordingly.

## 6. The middle slot

The Accounts section's middle slot resolves at render time:

| `compose_active` | `accounts_expanded` | Middle slot |
|---|---|---|
| `true` | (irrelevant) | `Form { ... }` |
| `false` | `true` | `List { items: [affordance] }` |
| `false` | `false` | (absent) |

When compose is active but Accounts is collapsed: the Form still shows (so the user can complete the draft they started). The Accounts content list is absent. After commit, the cursor moves into the new account row; the renderer expands the section as part of commit (existing T12 behavior).

## 7. Cleanup

The refactor eliminates several existing helpers and patterns:

- `find_accounts_header_followup_idx` — no longer needed; the affordance is a separate sub-List sibling.
- `models_header_idx` / `last_model_idx_per_account` insertion-offset computation — empty-catalog decorations are just ListItems prepended/appended within the ModelsSection content list.
- `selected = selected.map(|s| if s >= insert_idx { s + N } else { s })` bookkeeping after each decoration insert — gone. Each sub-List computes `selected` from cursor, once.
- The `compose_active`-gated `Frame → Stack[ Form, List ]` branch in the existing renderer — replaced by `Form` living in the Accounts section's middle slot.

## 8. What stays the same

- `visible_rows::enumerate` is unchanged. Same projection.
- `RowKind` enum unchanged.
- View enum, ListItem, FormRow, FormValue — all unchanged.
- View translator (`render_form`, `render_list`, `render_stack`, `render_frame`) — unchanged. Stack handles vertical layout; List handles items; Frame handles chrome.
- Selection (`ui/settings/focused`), j/k commands, dispatcher — all unchanged.
- Snapshots' rendered bytes are mostly unchanged (same items, same order; the Stack just gives more structure). View-tree assertion tests against the old `Frame → List` shape need updates.

## 9. Risks

- **Snapshot churn.** Insta-snapshot tests that match exact bytes should still pass (the rendered TUI output is byte-identical for layouts that fit; section boundaries don't introduce visible gaps). View-tree structural tests (e.g., `index_renderer_emits_frame_list_when_compose_inactive`) match against the old shape and need rewrites.
- **Stack rendering edge cases.** With several `Min(0)` children, the Stack translator's distribution might not match the current single-List behavior. Mitigation: snapshot tests catch any byte-level regression; the translator already handles Min(0) for the existing `Frame → Stack[ Form, List ]` case (T13).
- **Selection computation across sub-Lists.** Each sub-List independently checks "does the cursor's path match one of my items?" If implemented as O(items) per sub-List per frame, the cost scales with total items × number of sub-Lists — still small (~tens of items, ~4 sub-lists). No performance concern at current scale.

## 10. Execution

A single-task refactor — the structural change is cohesive enough that splitting it would just create knowingly-broken intermediate states. Plan at `docs/superpowers/plans/2026-05-14-settings-page-as-section-stack.md`.
