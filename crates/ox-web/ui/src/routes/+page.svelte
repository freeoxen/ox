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
  import { loadKeys, hasAnyKeys } from "$lib/stores/api-keys";
  import { addUsage } from "$lib/stores/usage";
  import type { OxAgent } from "$lib/wasm";
  import type { AgentEvent, DebugContext } from "$lib/types";

  import Chat from "$lib/components/Chat.svelte";
  import StatusBar from "$lib/components/StatusBar.svelte";
  import InputRow from "$lib/components/InputRow.svelte";
  import SettingsModal from "$lib/components/SettingsModal.svelte";
  import ModelSelector from "$lib/components/ModelSelector.svelte";
  import DebugPanel from "$lib/components/DebugPanel.svelte";
  import ToolPanel from "$lib/components/ToolPanel.svelte";
  import RequestLog from "$lib/components/RequestLog.svelte";

  let agent: OxAgent | null = $state(null);
  let inputDisabled = $state(true);
  let showSettings = $state(false);
  let pendingProvider: string | null = $state(null);
  let debugContext: DebugContext | null = $state(null);

  const PROVIDER_STORAGE_KEY = "ox:provider";

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

    // Show settings if no keys configured
    if (!hasAnyKeys()) {
      showSettings = true;
      // Wait for settings to close
      await new Promise<void>((resolve) => {
        const unsub = setInterval(() => {
          if (!showSettings) {
            clearInterval(unsub);
            resolve();
          }
        }, 50);
      });
    }

    // Create agent with anthropic key (backward compat)
    const finalKeys = loadKeys();
    const ag = wasm.create_agent(systemPrompt, finalKeys.anthropic ?? "");
    agent = ag;
    agentStore.set(ag);

    // Push all stored keys to the agent
    for (const [provider, key] of Object.entries(finalKeys)) {
      if (key) ag.set_api_key(provider, key);
    }

    // Restore persisted provider
    const storedProvider =
      localStorage.getItem(PROVIDER_STORAGE_KEY) || "anthropic";
    ag.set_provider(storedProvider);

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
        case "usage": {
          try {
            const u = JSON.parse(event.data);
            addUsage(u.input_tokens || 0, u.output_tokens || 0);
          } catch (_) {
            /* empty */
          }
          break;
        }
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

    if (hasAnyKeys()) {
      clearMessages();
      appendLine('Ready. Try: "reverse the word hello"', "system");
      inputDisabled = false;
    } else {
      appendLine(
        "No API key provided. Open settings to add your key.",
        "system",
      );
    }
  });

  function handleRequestKey(provider: string) {
    pendingProvider = provider;
    showSettings = true;
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

{#if showSettings && agent}
  <SettingsModal
    {agent}
    highlight={pendingProvider ?? undefined}
    onclose={() => {
      showSettings = false;
      // If opened for a specific provider and key is now set, notify ModelSelector
      // by dispatching a custom event it can listen for
      if (pendingProvider) {
        const keys = loadKeys();
        if (keys[pendingProvider] && agent) {
          // Push the new key to the agent
          agent.set_api_key(pendingProvider, keys[pendingProvider]!);
        }
        pendingProvider = null;
      }
      if (hasAnyKeys() && inputDisabled) {
        clearMessages();
        appendLine('Ready. Try: "reverse the word hello"', "system");
        inputDisabled = false;
      }
    }}
  />
{/if}

<input type="checkbox" id="debug-toggle" class="debug-toggle-checkbox" />

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
        <ModelSelector {agent} onrequestkey={handleRequestKey} />
      {/if}
      <button
        class="settings-btn"
        aria-label="Settings"
        onclick={() => (showSettings = true)}
      >
        <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
          <path
            fill="currentColor"
            d="M2 4h9v1H2V4zm11 0h1v1h-1V4zM2 7.5h3v1H2v-1zm5 0h7v1H7v-1zM2 11h7v1H2v-1zm9 0h3v1h-3v-1z"
          />
        </svg>
      </button>
      <label
        for="debug-toggle"
        class="debug-toggle-btn"
        aria-label="Toggle debug panels"
      >
        <svg width="18" height="18" viewBox="0 0 18 14" aria-hidden="true">
          <path
            fill="currentColor"
            d="M0 14 C1 9 3.5 5 6 5 C7.5 5 8 7.5 9 7.5 C10 7.5 10.5 2 13 2 C15.5 2 17 9 18 14Z"
          />
        </svg>
      </label>
    </div>
    <Chat />
    <StatusBar />
    <InputRow disabled={inputDisabled} onsend={handleSend} />
  </div>
  <label for="debug-toggle" class="debug-overlay"></label>
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
