# ox-gate Agent Loop Walkthrough

This document traces the agentic loop step-by-step under the proposed ox-gate
design, identifying what works, what breaks, and what needs resolution.

## Current Loop (Reference)

### Native (`Agent::prompt` via `Kernel::run_turn`)

```
1. agent.prompt("Hello")
2. Write user message → history/append
3. kernel.run_turn(context, transport, tools, emit):
   loop {
     4. Read path!("prompt")
        → Namespace.synthesize_prompt()
        → reads system/, history/messages, tools/schemas, model/id, model/max_tokens
        → assembles CompletionRequest { model, max_tokens, system, messages, tools, stream }
     5. transport.send(request) → EventStream
     6. accumulate_response(stream) → Vec<ContentBlock>
     7. Write assistant message → history/append
     8. Extract tool calls from ContentBlocks
     9. If no tool calls → return
    10. Execute each tool: tool.execute(input) → Result<String, String>
    11. Write tool results → history/append
    12. Loop to step 4
   }
```

### Wasm (`run_agentic_loop`, async, shell-driven)

```
1. Write user message → history/append
2. Read model/provider → "anthropic" | "openai"
3. Look up API key from HashMap
   loop {
     4. Read path!("prompt") → CompletionRequest
     5. match provider:
          "openai"  → await fetch_openai_completion()
          _         → await fetch_anthropic_completion()
     6. Parse SSE events (provider-specific)
     7. PreloadedTransport wraps events
     8. kernel.stream_once(request, preloaded, emit) → Vec<ContentBlock>
     9. Write assistant message → history/append
    10. Extract tool calls
    11. If no tool calls → return text
    12. Execute each tool (sync, including JS tools)
    13. Write tool results → history/append
    14. Loop to step 4
   }
```

Key difference: wasm shell drives the fetch (async) and tool-call loop
externally. Kernel only does accumulation via `stream_once`.

---

## Proposed Loop with ox-gate

### The "bootstrap as tool" proposal

The latest discussion proposes that the bootstrap completion (the kernel's
primary model call) should be "just another tool" — as unspecial as possible.
Let me trace what that means.

#### Attempt 1: Bootstrap as a literal Tool::execute call

```
1. Write user message → history/append
2. Read path!("prompt") → CompletionRequest
3. default_tool = tools.get(read("config/default_completion_tool"))
4. response = default_tool.execute(prompt_as_json, context)
   Inside execute:
     a. Write CompletionRequest to gate/accounts/{name}/complete → handle path
     b. Read gate/handles/{id} → page of events
     c. Read gate/handles/{id}/after/N → next page
     d. ... until no links.next
     e. Accumulate events into ContentBlocks
     f. Return accumulated text as String
5. ??? — the kernel has a String, not ContentBlocks
6. Cannot extract tool calls from a String
```

**Problem 1: Tool::execute returns `Result<String, String>`.** The kernel
needs `Vec<ContentBlock>` to extract tool calls (text blocks AND tool_use
blocks). A string result loses the structured content. The kernel can't tell
the model "you called these tools, here are the results" because it never sees
the tool calls.

If the tool returns JSON-serialized ContentBlocks, the kernel would need to
know that this specific tool's result is structured differently from all other
tools. That breaks the "unspecial" goal.

**Problem 2: In wasm, `Tool::execute` is synchronous.** Steps 4a-4d inside
execute require reading from gate, which reads from the HTTP store, which
does async I/O. In native Rust, reads can block. In wasm, they cannot.
`Tool::execute` would need to be async, which changes the trait for all tools.

**Problem 3: The tool needs the CompletionRequest, not just a prompt string.**
The tool schema shows `{ prompt: string, model?: string }`. But the kernel
needs to send the full CompletionRequest (system prompt, history, tool schemas,
model, max_tokens, stream flag). Either:
- The tool receives the full request (the schema lies about the input shape)
- The tool constructs the request internally (it needs access to history,
  system prompt, etc. — duplicating prompt synthesis)
- The tool receives just a prompt and constructs a minimal sub-request
  (fine for delegation, wrong for bootstrap)

#### Attempt 2: Bootstrap uses same infrastructure, different call site

What if "unspecial" means the infrastructure is shared, but the call site
differs?

```
1. Write user message → history/append
   loop {
     2. Read path!("prompt") → CompletionRequest
     3. Write CompletionRequest to gate/accounts/{default}/complete → handle path
     4. Read pages from handle, accumulate into ContentBlocks
        (same fn a completion tool would call internally)
     5. Write assistant message → history/append
     6. Extract tool calls
     7. If no tool calls → return
     8. Execute tools:
          - Regular tool: tool.execute(input, context) → String
          - Completion tool: tool.execute(input, context) → String
            (internally does steps 3-4 for a DIFFERENT account,
             returns accumulated TEXT only — tool calls from sub-model
             are not surfaced to the parent kernel)
     9. Write tool results → history/append
    10. Loop to step 2
   }
```

This works for native. The kernel's primary path (steps 3-4) and the
completion tool's execute (step 8) use the same write-to-gate, read-pages,
accumulate function. The only difference:

- **Bootstrap**: accumulates into ContentBlocks, processes tool calls, loops
- **Delegation tool**: accumulates into ContentBlocks, extracts text, returns

The bootstrap is "unspecial" in mechanism (same gate path, same codecs, same
handle protocol) but necessarily special in role (it's the one whose tool
calls get processed).

### What about wasm?

The wasm case needs async yields between page reads. Trace:

```
1. Write user message → history/append
   loop {
     2. Read path!("prompt") → CompletionRequest
     3. context.write(gate/accounts/{default}/complete, request)
        Inside gate:
          a. Resolve account → provider → codec
          b. codec.encode_request(request, account) → HttpRequest
          c. Write HttpRequest to services/http/request
             Inside HTTP store:
               - Queue the request
               - Return handle path "http/handles/xyz789"
               *** NO FETCH YET — that requires async ***
          d. Create gate handle mapping: a1b2c3 → xyz789
          e. Return "gate/handles/a1b2c3"

     *** PROBLEM: The HTTP fetch hasn't happened. ***
     *** Reading gate/handles/a1b2c3 would read http/handles/xyz789 ***
     *** which has no data yet because the fetch is queued, not executed ***

     4. Shell needs to actually perform the HTTP fetch here (async)
     5. Shell writes response data into HTTP store
     6. NOW shell can read gate/handles/a1b2c3 → page of events
     7. Process events, emit AgentEvents
     8. Read next page (may need another async yield if HTTP store
        is consuming a streaming response incrementally)
     9. ... until no links.next
    10. Write assistant message → history/append
    11. Execute tools
        - Regular tools: synchronous, fine
        - Completion tools: *** SAME ASYNC PROBLEM ***
          The tool's execute would need to initiate another HTTP fetch
          inside a synchronous call. This doesn't work in wasm.
    12. Write tool results → history/append
    13. Loop
   }
```

**Problem 4: The HTTP store can't do async I/O in a synchronous write.** In
wasm, `fetch()` is async. The StructFS `Writer::write` is synchronous. The
HTTP store can queue the request in its write, but someone needs to drive the
actual fetch.

Two approaches:

**4a. Shell drives the fetch externally.** The write to the HTTP store queues
the request. The shell (ox-web's async code) picks up the queued request, does
the fetch, and writes the response back into the HTTP store. Then reads from
the handle work.

This is structurally what happens today: the shell does `await fetch_*()` and
then preloads events. The difference is that with gate, the shell doesn't know
about providers or codecs — it just executes whatever HTTP request is queued.

```rust
// Shell async loop (ox-web)
loop {
    // Step 1: Let kernel/gate prepare the request
    kernel.prepare_request(&mut ctx)?;  // writes to gate, gate queues HTTP

    // Step 2: Shell executes the queued HTTP request
    let http_request = http_store.take_pending_request()?;
    let response = fetch(http_request).await?;
    http_store.deliver_response(response);

    // Step 3: Let kernel read the response pages
    let content = kernel.process_response(&mut ctx, &mut emit)?;

    // Step 4: Write assistant message
    kernel.write_assistant(&mut ctx, &content)?;

    if no_tool_calls(&content) { break; }

    // Step 5: Execute tools
    for tc in tool_calls(&content) {
        let result = tools.get(&tc.name).execute(tc.input, &mut ctx);
        // *** If this is a completion tool, it can't do async fetch ***
    }

    kernel.write_tool_results(&mut ctx, &results)?;
}
```

This works for the bootstrap completion but breaks for completion-as-tool
delegation (step 5). A completion tool calling gate would queue another HTTP
request, but there's no async yield point inside tool execution to drive it.

**4b. Completion tools in wasm return pending handles.** Instead of
blocking until done, a completion tool's execute returns immediately with a
handle path. The shell checks for pending handles after tool execution,
drives their fetches, then reads and accumulates. But this changes Tool::execute
semantics significantly — a tool can now return "I'm not done, drive this
handle."

---

## Problem Summary

### Problem 1: Tool::execute returns String, kernel needs ContentBlocks

**Severity:** Breaks bootstrap-as-tool entirely.

**Resolution options:**
- (a) Bootstrap is NOT a tool call. It uses the same gate infrastructure but
  the kernel drives it directly. The bootstrap is "unspecial" in mechanism,
  necessarily special in role.
- (b) Tool::execute returns a richer type (enum of String | ContentBlocks).
  But this leaks completion semantics into the general tool trait.
- (c) A new "CompletionTool" trait alongside Tool. But this is specialization,
  not "unspecial."

**Recommendation:** (a). The bootstrap completion uses the same gate path
(write to `gate/accounts/*/complete`, read pages, accumulate). Delegation
completion tools also use that path, but return text only. The kernel drives
the bootstrap; tools drive delegations.

### Problem 2: Tool::execute is synchronous, wasm needs async

**Severity:** Breaks delegation completion tools in wasm.

**Resolution options:**
- (a) Delegation tools only work in native (where reads can block). In wasm,
  only the bootstrap completion works (shell drives the async). Delegation
  is a future capability gated on Isotope (where blocks can block on reads).
- (b) Tool::execute becomes async. Major trait change, infects all tools.
- (c) Tools can return "pending" results. Shell drives pending completions.
  Complex but doesn't infect the trait.
- (d) Delegation tools in wasm use a synchronous HTTP fallback (non-streaming,
  blocking via `XMLHttpRequest` synchronous mode). Hacky but pragmatic.

**Recommendation:** (a) for now. Delegation works in native. In wasm, the
bootstrap is the only completion the kernel drives. Multi-model delegation
is deferred to when the runtime supports blocking reads (Isotope). This is
honest about the platform constraint rather than papering over it.

### Problem 3: Completion tool input schema

**Severity:** Design gap, not a blocker.

**Resolution:** For delegation, the tool receives a prompt string and
constructs a minimal CompletionRequest (system prompt from the tool's config,
the prompt as the sole user message, no tools — the sub-model doesn't get to
call tools). For bootstrap, the kernel constructs the full CompletionRequest
via prompt synthesis and sends it directly — not through the tool schema.

### Problem 4: HTTP store async I/O in wasm

**Severity:** Architectural, must resolve for gate to work in wasm.

**Resolution:** The HTTP store in wasm is a two-phase store:

- **Write phase** (synchronous): queue the HTTP request, return a handle path
- **Shell drives** (async): `take_pending() → fetch() → deliver_response()`
- **Read phase** (synchronous): return response chunks from the handle

The shell interleaves these phases. Gate is unaware of the async boundary —
it writes to HTTP and reads from HTTP. The shell sits in between, driving
the actual fetch between gate's write and read.

```
gate.write(request)        ← synchronous, queues in HTTP store
  ↓
shell: await fetch(...)    ← async, shell-driven
  ↓
shell: http.deliver(resp)  ← synchronous, writes response into store
  ↓
gate.read(handle)          ← synchronous, reads from HTTP store
```

This maps cleanly to the Isotope model where the runtime handles
cross-block I/O transparently. Pre-Isotope, the shell plays the runtime's
role manually.

---

## Revised Loop Design

### Native

```
1. Write user message → history/append
   loop {
     2. Read path!("prompt") → CompletionRequest
        Namespace.synthesize_prompt() reads:
          system/         → SystemProvider (unchanged)
          history/messages → HistoryProvider (unchanged)
          tools/schemas   → ToolsProvider (now includes completion tools from gate)
          model/id        → gate/accounts/{default}/model (or thin proxy)
          model/max_tokens → gate/accounts/{default}/max_tokens (or thin proxy)
     3. context.write(gate/accounts/{default}/complete, request) → handle
     4. Read pages from handle until done → Vec<StreamEvent>
        (gate reads from HTTP store, translates via codec, returns pages)
     5. Accumulate events into ContentBlocks
     6. Write assistant message → history/append
     7. Extract tool calls
     8. If no tool calls → return
     9. Execute tools:
          - Regular tools: tool.execute(input, context) → String
          - Completion tools: tool.execute(input, context)
              → internally does steps 3-5 for a different account
              → returns accumulated text
    10. Write tool results → history/append
    11. Loop to step 2
   }
```

Steps 3-5 are a shared function: `complete_via_gate(context, account, request)
→ Vec<ContentBlock>`. Both the kernel's bootstrap path and the completion
tool's execute call it. The kernel extracts tool calls; the tool extracts text.

### Wasm

```
1. Write user message → history/append
   loop {
     2. Read path!("prompt") → CompletionRequest
     3. context.write(gate/accounts/{default}/complete, request) → handle
        (gate writes to HTTP store → queued, not fetched)
     4. Shell: pending_request = http_store.take_pending()
     5. Shell: response = await fetch(pending_request)
     6. Shell: http_store.deliver_response(response)
     7. Read pages from handle → Vec<StreamEvent>
        (page reads are synchronous — data is already in HTTP store)
        (for streaming: shell may need to deliver chunks incrementally,
         reading pages between deliveries)
     8. Accumulate events into ContentBlocks
     9. Write assistant message → history/append
    10. Extract tool calls
    11. If no tool calls → return
    12. Execute tools:
          - Regular tools: tool.execute(input, context) → String
          - Completion tools: NOT AVAILABLE in wasm
            (would require async inside synchronous execute)
    13. Write tool results → history/append
    14. Loop to step 2
   }
```

The wasm loop is structurally similar to today, but the provider dispatch
and codec translation now live inside gate rather than in the shell. The
shell's job shrinks to: drive HTTP fetches between synchronous kernel steps.

### Streaming in wasm (incremental page reads)

For text streaming (showing tokens as they arrive), the shell needs to read
pages incrementally as the HTTP response streams in:

```
3. context.write(gate/accounts/{default}/complete, request) → handle
4. Shell: initiate fetch (non-blocking)
   loop {
     5. Shell: await next chunk from fetch response
     6. Shell: http_store.deliver_chunk(chunk)
     7. Read gate/handles/{id}/after/{cursor} → page of events
        (gate reads new chunks from HTTP store, translates, returns page)
     8. Process events (emit TextDelta to UI)
     9. If page has links.next → continue
     10. If no links.next → break
   }
```

This gives token-by-token streaming without blocking. The shell interleaves
chunk delivery with page reads. Each page read is synchronous and returns
a coherent set of events.

---

## What Needs Fixing in the Design Doc

### 1. The bootstrap cannot literally be a Tool::execute call

The design doc should clarify: the bootstrap completion uses the same gate
infrastructure (same write path, same handle protocol, same codecs) as
completion tools. The kernel drives it directly. This is "unspecial" in
mechanism, but necessarily distinct in call site — the kernel calls gate
directly for bootstrap, tools call gate for delegation.

The kernel has ONE completion function that both paths use:

```rust
fn complete_via_gate(
    context: &mut dyn Store,
    account_path: &Path,
    request: &CompletionRequest,
    emit: &mut dyn FnMut(AgentEvent),
) -> Result<Vec<ContentBlock>, String>
```

The kernel calls this directly. Completion tools call this inside their
execute. Same function, different call sites.

### 2. Prompt synthesis needs a model source

The design doc should specify where prompt synthesis reads model config:

**Option A — Gate serves model paths.** Mount gate at both `gate/` and `model/`
(rename mount), or gate responds to model/* paths internally. Prompt synthesis
is unchanged — reads `model/id`, gets the default account's model.

**Option B — Thin proxy.** A `GateModelProxy` store is mounted at `model/`.
On read, it reads from the gate's default account config. Prompt synthesis
unchanged.

**Option C — Prompt synthesis reads from gate directly.** Synthesize_prompt
reads `gate/accounts/{default}/model` instead of `model/id`. Changes prompt
synthesis code but eliminates the proxy.

**Recommendation:** B for phase 2 (minimal changes to prompt synthesis), with
a path to C in phase 3 (when prompt synthesis is refactored alongside
Transport retirement).

### 3. Tool list composition

The design doc should specify how completion tool schemas merge into the
tool list:

- Gate exposes `gate/tools/schemas` → array of completion tool schemas
- On account changes, gate updates this list
- `ToolsProvider` becomes writable or is replaced with a composite store that
  reads from both the shell's tool registry AND gate's tool schemas
- Prompt synthesis reads `tools/schemas` and gets the merged list

### 4. Completion tools are native-only for now

The design doc should explicitly state that completion-as-tool delegation
requires blocking reads. In wasm (pre-Isotope), only the bootstrap completion
works — the shell drives the async. Delegation tools are registered in the
tool list (so the model sees them) but return an error in wasm:
`"Delegation to other models requires native runtime or Isotope"`.

This is honest about the platform constraint and sets up the Isotope migration
cleanly — when reads can block, the same tool code works unchanged.

### 5. Kernel simplification

With these clarifications, the kernel's surface area is:

```rust
impl Kernel {
    /// Run one full turn. Reads prompt, calls gate, processes tool calls, loops.
    pub fn run_turn(
        &mut self,
        context: &mut dyn Store,
        tools: &ToolRegistry,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String>
}
```

No `stream_once`. No `begin_completion` / `read_page`. No Transport generic.
The kernel reads and writes the Store. Period.

For wasm, the kernel STILL has `run_turn` but the HTTP store underneath is
two-phase (queue on write, deliver externally, serve on read). The kernel
doesn't know about this — the shell wraps the namespace with async delivery
between the kernel's synchronous reads and writes.

**However:** this means `run_turn` can't be called as a single synchronous
function in wasm, because the HTTP fetch must happen between the gate write
and the gate read. Two options:

**(a)** Split `run_turn` into phases that the shell drives:

```rust
pub fn prepare_turn(&mut self, context: &mut dyn Store) -> Result<Path, String>
  // reads prompt, writes to gate, returns handle path

pub fn process_page(&mut self, context: &mut dyn Store, handle: &Path, emit: ...)
  -> Result<PageResult, String>
  // reads one page, accumulates, returns whether more pages / tool calls / done

pub fn execute_tools(&mut self, context: &mut dyn Store, tools: &ToolRegistry, emit: ...)
  -> Result<(), String>
  // executes pending tool calls, writes results
```

The shell calls these in sequence with async yields between them.

**(b)** The kernel calls `run_turn` as a single call, but the Store
implementation (the namespace + gate + HTTP store stack) internally handles
the async. This requires the Store to support async — which it doesn't today,
but could in an Isotope runtime.

**Recommendation:** (a) for pre-Isotope wasm, (b) when Isotope provides
async store semantics. The phase-split approach is explicit about what needs
async and keeps the kernel synchronous.

---

## Summary

| Aspect | Works? | Notes |
|--------|--------|-------|
| Gate as Store (provider/account/handle) | Yes | Core architecture is sound |
| Codecs as gate-internal | Yes | Clean separation |
| HTTP store (shell-provided) | Yes | Two-phase for wasm |
| Bootstrap completion via gate | Yes | Kernel drives directly |
| Completion tools (native) | Yes | Same function, different call site |
| Completion tools (wasm) | No | Needs blocking reads (Isotope) |
| Prompt synthesis model source | Needs work | Thin proxy or gate serves model/* |
| Tool list composition | Needs work | Composite tools store |
| Kernel::run_turn (native) | Yes | Drop Transport, use Store |
| Kernel::run_turn (wasm) | Needs split | Phase-based for async interleaving |
| "Bootstrap as unspecial" | Partially | Same mechanism, different call site |
| Token streaming (wasm) | Yes | Shell delivers chunks, reads pages |
