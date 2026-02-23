import { debuggerEl } from './dom';
import { makeEmpty, makeSection, makeKV } from './dom-helpers';
import type { OxAgent } from '/pkg/ox_web.js';
import type { DebugContext, DebugMessage, DebugContentBlock } from './types';

export function refreshDebugger(agent: OxAgent): void {
  const raw = agent.debug_context();
  if (!raw) return;
  const ctx: DebugContext = JSON.parse(raw);
  debuggerEl.textContent = '';

  // /system (editable)
  debuggerEl.appendChild(makeSystemSection(agent, ctx.system));

  // /model
  debuggerEl.appendChild(
    makeSection(
      '/model',
      () => {
        const body = document.createElement('div');
        body.className = 'section-body';
        const m = ctx.model || {};
        body.appendChild(makeKV('id', m.id, 'str'));
        body.appendChild(makeKV('max_tokens', m.max_tokens, 'num'));
        return body;
      },
      true,
    ),
  );

  // /tools
  const tools = ctx.tools || [];
  debuggerEl.appendChild(
    makeSection('/tools (' + tools.length + ')', () => {
      const body = document.createElement('div');
      body.className = 'section-body';
      if (tools.length === 0) {
        body.appendChild(makeEmpty('none'));
        return body;
      }
      for (const t of tools) {
        const entry = document.createElement('details');
        const sum = document.createElement('summary');
        const nameSpan = document.createElement('span');
        nameSpan.className = 'tool-name';
        nameSpan.textContent = t.name;
        sum.appendChild(nameSpan);
        entry.appendChild(sum);
        const inner = document.createElement('div');
        inner.className = 'section-body';
        inner.appendChild(makeKV('description', t.description, 'str'));
        const schemaLine = document.createElement('div');
        schemaLine.className = 'kv';
        const sk = document.createElement('span');
        sk.className = 'k';
        sk.textContent = 'input_schema';
        const sv = document.createElement('pre');
        sv.className = 'v';
        sv.textContent = JSON.stringify(t.input_schema, null, 2);
        sv.style.margin = '0';
        sv.style.whiteSpace = 'pre-wrap';
        schemaLine.appendChild(sk);
        schemaLine.appendChild(sv);
        inner.appendChild(schemaLine);
        entry.appendChild(inner);
        body.appendChild(entry);
      }
      return body;
    }),
  );

  // /history
  const hist = ctx.history || {};
  const count = hist.count || 0;
  const messages = hist.messages || [];
  debuggerEl.appendChild(
    makeSection(
      '/history (' + count + ')',
      () => {
        const body = document.createElement('div');
        body.className = 'section-body';
        if (messages.length === 0) {
          body.appendChild(makeEmpty('empty'));
          return body;
        }
        for (let i = 0; i < messages.length; i++) {
          body.appendChild(renderMessage(i, messages[i]));
        }
        return body;
      },
      true,
    ),
  );
}

function makeSystemSection(
  agent: OxAgent,
  systemVal: string | null,
): HTMLDetailsElement {
  const details = document.createElement('details');
  details.open = true;
  const summary = document.createElement('summary');
  const header = document.createElement('span');
  header.className = 'section-header';
  header.textContent = '/system ';
  const editBtn = document.createElement('button');
  editBtn.className = 'edit-btn';
  editBtn.textContent = 'edit';
  editBtn.addEventListener('click', (e) => {
    e.preventDefault();
    e.stopPropagation();
    showEditMode();
  });
  header.appendChild(editBtn);
  summary.appendChild(header);
  details.appendChild(summary);

  const body = document.createElement('div');
  body.className = 'section-body';
  const fullText = systemVal != null ? String(systemVal) : '';

  function showReadMode(): void {
    body.textContent = '';
    if (fullText) {
      const pre = document.createElement('div');
      pre.className = 'str-val';
      pre.textContent =
        fullText.length > 200 ? fullText.slice(0, 200) + '...' : fullText;
      body.appendChild(pre);
    } else {
      body.appendChild(makeEmpty('null'));
    }
  }

  function showEditMode(): void {
    body.textContent = '';
    const textarea = document.createElement('textarea');
    textarea.className = 'system-textarea';
    textarea.value = fullText;
    body.appendChild(textarea);

    const actions = document.createElement('div');
    actions.className = 'edit-actions';
    const saveBtn = document.createElement('button');
    saveBtn.className = 'save-btn';
    saveBtn.textContent = 'save';
    saveBtn.addEventListener('click', () => {
      try {
        agent.set_system_prompt(textarea.value);
      } catch (err) {
        alert('Failed to save: ' + err);
      }
    });
    const cancelBtn = document.createElement('button');
    cancelBtn.className = 'cancel-btn';
    cancelBtn.textContent = 'cancel';
    cancelBtn.addEventListener('click', () => {
      refreshDebugger(agent);
    });
    actions.appendChild(saveBtn);
    actions.appendChild(cancelBtn);
    body.appendChild(actions);

    textarea.focus();
  }

  showReadMode();
  details.appendChild(body);
  return details;
}

function renderMessage(index: number, msg: DebugMessage): HTMLDivElement {
  const entry = document.createElement('div');
  entry.className = 'msg-entry';

  const role = msg.role || '?';
  const headerDiv = document.createElement('div');

  const idx = document.createElement('span');
  idx.className = 'text-muted';
  idx.textContent = '#' + index + ' ';

  headerDiv.appendChild(idx);

  // Detect tool_result messages
  let isToolResult = false;
  if (
    role === 'user' &&
    Array.isArray(msg.content) &&
    msg.content.length > 0 &&
    (msg.content[0] as DebugContentBlock).type === 'tool_result'
  ) {
    isToolResult = true;
  }

  const badge = document.createElement('span');
  badge.className = 'role-badge ';
  if (isToolResult) {
    badge.className += 'role-tool-result';
    badge.textContent = 'tool_result';
  } else if (role === 'assistant') {
    badge.className += 'role-assistant';
    badge.textContent = 'assistant';
  } else {
    badge.className += 'role-user';
    badge.textContent = 'user';
  }
  headerDiv.appendChild(badge);
  entry.appendChild(headerDiv);

  // Content
  const content = document.createElement('div');
  content.className = 'msg-content';

  if (isToolResult) {
    for (const r of msg.content as DebugContentBlock[]) {
      const line = document.createElement('div');
      const tid = document.createElement('span');
      tid.className = 'text-muted';
      tid.textContent = r.tool_use_id
        ? r.tool_use_id.slice(0, 12) + '...'
        : '?';
      line.appendChild(tid);
      const arrow = document.createElement('span');
      arrow.className = 'text-muted';
      arrow.textContent = ' \u2192 ';
      line.appendChild(arrow);
      const val = document.createElement('span');
      val.className = 'str-val';
      val.textContent = r.content || '';
      line.appendChild(val);
      content.appendChild(line);
    }
  } else if (role === 'assistant' && Array.isArray(msg.content)) {
    for (const block of msg.content as DebugContentBlock[]) {
      const line = document.createElement('div');
      if (block.type === 'text') {
        const tag = document.createElement('span');
        tag.className = 'block-tag';
        tag.textContent = '[text] ';
        line.appendChild(tag);
        const val = document.createElement('span');
        const t = block.text || '';
        val.textContent = t.length > 120 ? t.slice(0, 120) + '...' : t;
        line.appendChild(val);
      } else if (block.type === 'tool_use') {
        const tag = document.createElement('span');
        tag.className = 'block-tag';
        tag.textContent = '[tool_use] ';
        line.appendChild(tag);
        const nameEl = document.createElement('span');
        nameEl.className = 'tool-name';
        nameEl.textContent = block.name + ' ';
        line.appendChild(nameEl);
        const inputEl = document.createElement('span');
        inputEl.className = 'text-muted';
        inputEl.textContent = JSON.stringify(block.input);
        line.appendChild(inputEl);
      }
      content.appendChild(line);
    }
  } else {
    // Plain user message
    const text =
      typeof msg.content === 'string'
        ? msg.content
        : JSON.stringify(msg.content);
    content.textContent =
      text.length > 200 ? text.slice(0, 200) + '...' : text;
  }

  entry.appendChild(content);
  return entry;
}
