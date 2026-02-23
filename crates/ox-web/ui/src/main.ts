import init, { create_agent } from '/pkg/ox_web.js';
import { output, input, sendBtn } from './dom';
import { setStatus } from './status';
import { append, appendLine } from './chat';
import { refreshDebugger } from './debugger';
import { addRequestLogEntry, refreshRequestLog } from './request-log';
import { applyProfile, refreshToolPanel } from './tool-panel';
import { ToolStore } from './tool-store';
import type { AgentEvent } from './types';

async function main(): Promise<void> {
  await init('/pkg/ox_web_bg.wasm');

  const systemPrompt = [
    'You are a helpful assistant with access to a tool called reverse_text.',
    'When the user asks you to reverse text, use the reverse_text tool.',
    'After getting the tool result, report it to the user.',
  ].join(' ');

  const serverUrl = window.location.origin;
  const agent = create_agent(systemPrompt, serverUrl);

  // Initial debugger state
  refreshDebugger(agent);

  // Stream events to the output
  let currentAssistantSpan: HTMLSpanElement | null = null;
  agent.on_event(function (event: AgentEvent) {
    switch (event.type) {
      case 'turn_start':
        setStatus('thinking...', '');
        break;
      case 'request_sent': {
        let model = '';
        try {
          model = JSON.parse(event.data).model || '';
        } catch (_) {
          /* empty */
        }
        setStatus('prompting', model ? '(' + model + ')' : '');
        addRequestLogEntry(event.data);
        refreshRequestLog();
        break;
      }
      case 'text_delta':
        setStatus('streaming response...', '');
        if (!currentAssistantSpan) {
          currentAssistantSpan = document.createElement('span');
          currentAssistantSpan.className = 'assistant-msg';
          output.appendChild(currentAssistantSpan);
        }
        currentAssistantSpan.textContent += event.data;
        output.scrollTop = output.scrollHeight;
        break;
      case 'tool_call_start':
        setStatus('calling tool', event.data);
        currentAssistantSpan = null;
        appendLine('\n[tool call: ' + event.data + ']', 'tool-call');
        break;
      case 'tool_call_result':
        appendLine('[tool result: ' + event.data + ']', 'tool-result');
        break;
      case 'turn_end':
        setStatus('', '');
        currentAssistantSpan = null;
        append('\n');
        break;
      case 'error':
        setStatus('', '');
        currentAssistantSpan = null;
        appendLine('Error: ' + event.data, 'error');
        break;
      case 'context_changed':
        refreshDebugger(agent);
        break;
    }
  });

  // Restore persisted tools and render tool panel
  applyProfile(agent, ToolStore.getActiveProfile());
  refreshToolPanel(agent);

  output.textContent = '';
  appendLine('Ready. Try: "reverse the word hello"', 'system');

  input.disabled = false;
  sendBtn.disabled = false;
  input.focus();

  async function send(): Promise<void> {
    const text = input.value.trim();
    if (!text) return;

    input.value = '';
    input.disabled = true;
    sendBtn.disabled = true;
    currentAssistantSpan = null;

    appendLine('\n> ' + text, 'user-msg');

    try {
      await agent.prompt(text);
    } catch (e) {
      appendLine('Error: ' + e, 'error');
    }

    input.disabled = false;
    sendBtn.disabled = false;
    input.focus();
  }

  sendBtn.addEventListener('click', send);
  input.addEventListener('keydown', (e: KeyboardEvent) => {
    if (e.key === 'Enter') send();
  });
}

main().catch((e) => {
  output.textContent = 'Failed to load: ' + e;
});
