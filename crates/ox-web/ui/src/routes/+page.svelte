<script lang="ts">
  import { onMount } from "svelte";
  import { base } from "$app/paths";
  import { agent as agentStore } from "$lib/stores/agent";
  import {
    appendMessage,
    appendLine,
    appendToStream,
    flushStream,
    clearMessages,
  } from "$lib/stores/chat";
  import { setStatus } from "$lib/stores/status";
  import { addRequestLogEntry } from "$lib/stores/request-log";
  import { applyProfile, ToolStore } from "$lib/stores/tools";
  import type { OxAgent } from "$lib/wasm";
  import type { AgentEvent, DebugContext } from "$lib/types";

  import Chat from "$lib/components/Chat.svelte";
  import StatusBar from "$lib/components/StatusBar.svelte";
  import InputRow from "$lib/components/InputRow.svelte";
  import ApiKeyModal from "$lib/components/ApiKeyModal.svelte";
  import ModelSelector from "$lib/components/ModelSelector.svelte";
  import DebugPanel from "$lib/components/DebugPanel.svelte";
  import ToolPanel from "$lib/components/ToolPanel.svelte";
  import RequestLog from "$lib/components/RequestLog.svelte";

  let agent: OxAgent | null = $state(null);
  let inputDisabled = $state(true);
  let showApiKeyModal = $state(false);
  let apiKey: string | null = $state(null);
  let debugContext: DebugContext | null = $state(null);

  const API_KEY_STORAGE = "ox:api-key";

  function sleep(ms: number): Promise<void> {
    return new Promise((r) => setTimeout(r, ms));
  }

  onMount(async () => {
    setStatus("loading", "wasm");
    const started = Date.now();

    // @ts-ignore — resolved at runtime by Vite proxy / ox-dev-server
    const wasm = await import(/* @vite-ignore */ `${base}/pkg/ox_web.js`);
    await wasm.default(`${base}/pkg/ox_web_bg.wasm`);

    const MIN_SPINNER_MS = 400;
    const elapsed = Date.now() - started;
    if (elapsed < MIN_SPINNER_MS) await sleep(MIN_SPINNER_MS - elapsed);
    setStatus("", "");

    const systemPrompt = [
      "You are a helpful assistant with access to a tool called reverse_text.",
      "When the user asks you to reverse text, use the reverse_text tool.",
      "After getting the tool result, report it to the user.",
    ].join(" ");

    // Get API key from session storage or prompt user
    let storedKey = sessionStorage.getItem(API_KEY_STORAGE);
    if (!storedKey) {
      showApiKeyModal = true;
      // Wait for modal to resolve
      await new Promise<void>((resolve) => {
        const unsub = setInterval(() => {
          if (!showApiKeyModal) {
            clearInterval(unsub);
            resolve();
          }
        }, 50);
      });
      storedKey = apiKey;
    }
    if (storedKey) {
      sessionStorage.setItem(API_KEY_STORAGE, storedKey);
      apiKey = storedKey;
    }

    const ag = wasm.create_agent(systemPrompt, apiKey ?? "");
    agent = ag;
    agentStore.set(ag);

    // Read initial debug context
    try {
      const raw = ag.debug_context();
      if (raw) debugContext = JSON.parse(raw);
    } catch (_) {
      /* empty */
    }

    // Stream events
    ag.on_event(function (event: AgentEvent) {
      switch (event.type) {
        case "turn_start":
          setStatus("thinking...", "");
          break;
        case "request_sent": {
          let model = "";
          try {
            model = JSON.parse(event.data).model || "";
          } catch (_) {
            /* empty */
          }
          setStatus("prompting", model ? "(" + model + ")" : "");
          addRequestLogEntry(event.data);
          break;
        }
        case "text_delta":
          setStatus("streaming response...", "");
          appendToStream(event.data);
          break;
        case "tool_call_start":
          setStatus("calling tool", event.data);
          flushStream();
          appendLine("\n[tool call: " + event.data + "]", "tool-call");
          break;
        case "tool_call_result":
          appendLine("[tool result: " + event.data + "]", "tool-result");
          break;
        case "turn_end":
          setStatus("", "");
          flushStream();
          appendMessage("\n", "");
          break;
        case "error":
          setStatus("", "");
          flushStream();
          appendLine("Error: " + event.data, "error");
          break;
        case "context_changed":
          try {
            const raw = ag.debug_context();
            if (raw) debugContext = JSON.parse(raw);
          } catch (_) {
            /* empty */
          }
          break;
      }
    });

    // Restore persisted tools
    applyProfile(ag, ToolStore.getActiveProfile());

    // Read debug context after tool registration
    try {
      const raw = ag.debug_context();
      if (raw) debugContext = JSON.parse(raw);
    } catch (_) {
      /* empty */
    }

    if (apiKey) {
      clearMessages();
      appendLine('Ready. Try: "reverse the word hello"', "system");
      inputDisabled = false;
    } else {
      appendLine(
        "No API key provided. Enter your key to use the playground.",
        "system",
      );
    }
  });

  function handleApiKeySubmit(key: string) {
    apiKey = key;
    sessionStorage.setItem(API_KEY_STORAGE, key);
    showApiKeyModal = false;
    if (agent) agent.set_api_key(key);
  }

  function handleApiKeyCancel() {
    showApiKeyModal = false;
  }

  async function handleSend(text: string) {
    if (!agent) return;
    inputDisabled = true;
    flushStream();
    appendLine("\n> " + text, "user-msg");

    try {
      await agent.prompt(text);
    } catch (e) {
      appendLine("Error: " + e, "error");
    }

    inputDisabled = false;
  }
</script>

{#if showApiKeyModal}
  <ApiKeyModal onsubmit={handleApiKeySubmit} oncancel={handleApiKeyCancel} />
{/if}

<div class="container">
  <div class="chat-column">
    <div class="header-row">
      <h1>
        {#if base}<a href="/" style="color:inherit;text-decoration:none"
            >ox<span
              style="font-size:.2em;vertical-align:2.5em;margin-left:.15em"
              >tm</span
            ></a
          >{:else}ox<span
            style="font-size:.2em;vertical-align:2.5em;margin-left:.15em"
            >tm</span
          >{/if}
        <span>playground</span>
      </h1>
      {#if agent}
        <ModelSelector {agent} />
      {/if}
    </div>
    <Chat />
    <StatusBar />
    <InputRow disabled={inputDisabled} onsend={handleSend} />
  </div>
  <div class="debug-column">
    <div>
      <div class="debug-header">
        <h2>context</h2>
        <span class="path">/</span>
      </div>
      <div class="debug-panel">
        {#if agent}
          <DebugPanel {agent} {debugContext} />
        {:else}
          <div class="panel-loading"><div class="spinner"></div></div>
        {/if}
      </div>
    </div>
    <div>
      <div class="debug-header">
        <h2>tools</h2>
      </div>
      <div class="debug-panel">
        {#if agent}
          <ToolPanel {agent} />
        {:else}
          <div class="panel-loading"><div class="spinner"></div></div>
        {/if}
      </div>
    </div>
    <div>
      <div class="debug-header">
        <h2>request log</h2>
      </div>
      <div class="debug-panel">
        <RequestLog />
      </div>
    </div>
  </div>
</div>
