import init, { create_agent } from "/pkg/ox_web.js";
import { output, input, sendBtn } from "./dom";
import { setStatus } from "./status";
import { append, appendLine } from "./chat";
import { refreshDebugger } from "./debugger";
import { addRequestLogEntry, refreshRequestLog } from "./request-log";
import { applyProfile, refreshToolPanel } from "./tool-panel";
import { ToolStore } from "./tool-store";
import { initThemePicker } from "./theme";
import { getStoredApiKey, storeApiKey, showApiKeyPrompt } from "./connection";
import type { AgentEvent } from "./types";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function main(): Promise<void> {
  initThemePicker();
  const started = Date.now();
  await init("/pkg/ox_web_bg.wasm");

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
  let apiKey = getStoredApiKey();
  if (!apiKey) {
    apiKey = await showApiKeyPrompt();
  }
  if (apiKey) {
    storeApiKey(apiKey);
  }

  const agent = create_agent(systemPrompt, apiKey ?? "");

  // Model selector
  const MODEL_STORAGE_KEY = "ox:model";
  const DEFAULT_MODEL = "claude-sonnet-4-20250514";

  const modelSelect = document.createElement("select");
  modelSelect.className = "model-select";

  // Start with the default model while catalog loads
  const defaultOpt = document.createElement("option");
  defaultOpt.value = DEFAULT_MODEL;
  defaultOpt.textContent = DEFAULT_MODEL;
  modelSelect.appendChild(defaultOpt);

  const stored = sessionStorage.getItem(MODEL_STORAGE_KEY);
  if (stored && stored !== DEFAULT_MODEL) {
    const storedOpt = document.createElement("option");
    storedOpt.value = stored;
    storedOpt.textContent = stored;
    modelSelect.appendChild(storedOpt);
    modelSelect.value = stored;
    agent.set_model(stored);
  }

  modelSelect.addEventListener("change", () => {
    sessionStorage.setItem(MODEL_STORAGE_KEY, modelSelect.value);
    agent.set_model(modelSelect.value);
  });

  document.querySelector(".header-row")!.appendChild(modelSelect);

  // Fetch model catalog from Anthropic API (non-blocking)
  if (apiKey) {
    agent
      .refresh_models()
      .then((catalogJson: string) => {
        const catalog: { id: string; display_name: string }[] =
          JSON.parse(catalogJson);
        if (catalog.length === 0) return;

        const current = modelSelect.value;
        modelSelect.innerHTML = "";
        for (const m of catalog) {
          const opt = document.createElement("option");
          opt.value = m.id;
          opt.textContent = m.display_name;
          modelSelect.appendChild(opt);
        }
        // Restore selection if still in catalog, otherwise keep default
        if (catalog.some((m) => m.id === current)) {
          modelSelect.value = current;
        } else {
          modelSelect.value = DEFAULT_MODEL;
          agent.set_model(DEFAULT_MODEL);
        }
      })
      .catch(() => {
        // Catalog fetch failed — keep the default option, not critical
      });
  }

  if (!apiKey) {
    appendLine(
      "No API key provided. Enter your key to use the playground.",
      "system",
    );
    input.disabled = true;
    sendBtn.disabled = true;
  }

  // Initial debugger state
  refreshDebugger(agent);

  // Stream events to the output
  let currentAssistantSpan: HTMLSpanElement | null = null;
  agent.on_event(function (event: AgentEvent) {
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
        refreshRequestLog();
        break;
      }
      case "text_delta":
        setStatus("streaming response...", "");
        if (!currentAssistantSpan) {
          currentAssistantSpan = document.createElement("span");
          currentAssistantSpan.className = "assistant-msg";
          output.appendChild(currentAssistantSpan);
        }
        currentAssistantSpan.textContent += event.data;
        output.scrollTop = output.scrollHeight;
        break;
      case "tool_call_start":
        setStatus("calling tool", event.data);
        currentAssistantSpan = null;
        appendLine("\n[tool call: " + event.data + "]", "tool-call");
        break;
      case "tool_call_result":
        appendLine("[tool result: " + event.data + "]", "tool-result");
        break;
      case "turn_end":
        setStatus("", "");
        currentAssistantSpan = null;
        append("\n");
        break;
      case "error":
        setStatus("", "");
        currentAssistantSpan = null;
        appendLine("Error: " + event.data, "error");
        break;
      case "context_changed":
        refreshDebugger(agent);
        break;
    }
  });

  // Restore persisted tools and render tool panel
  applyProfile(agent, ToolStore.getActiveProfile());
  refreshToolPanel(agent);
  refreshRequestLog();

  if (apiKey) {
    output.textContent = "";
    appendLine('Ready. Try: "reverse the word hello"', "system");

    input.disabled = false;
    sendBtn.disabled = false;
    input.focus();
  }

  async function send(): Promise<void> {
    const text = input.value.trim();
    if (!text) return;

    input.value = "";
    input.disabled = true;
    sendBtn.disabled = true;
    currentAssistantSpan = null;

    appendLine("\n> " + text, "user-msg");

    try {
      await agent.prompt(text);
    } catch (e) {
      appendLine("Error: " + e, "error");
    }

    input.disabled = false;
    sendBtn.disabled = false;
    input.focus();
  }

  sendBtn.addEventListener("click", send);
  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") send();
  });
}

main().catch((e) => {
  output.textContent = "Failed to load: " + e;
});
