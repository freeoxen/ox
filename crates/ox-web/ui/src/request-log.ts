import { requestLogEl } from './dom';
import { makeEmpty } from './dom-helpers';
import type { RequestLogEntry } from './types';

const requestLog: RequestLogEntry[] = [];

export function addRequestLogEntry(data: string): void {
  requestLog.push({ timestamp: new Date(), data });
}

export function refreshRequestLog(): void {
  requestLogEl.textContent = '';
  if (requestLog.length === 0) {
    requestLogEl.appendChild(makeEmpty('no requests yet'));
    return;
  }
  for (let i = 0; i < requestLog.length; i++) {
    const entry = requestLog[i];
    let parsed: { messages?: unknown[]; tools?: unknown[] } | null;
    try {
      parsed = JSON.parse(entry.data);
    } catch (_) {
      parsed = null;
    }
    const msgCount =
      parsed && parsed.messages ? parsed.messages.length : '?';
    const toolCount = parsed && parsed.tools ? parsed.tools.length : 0;
    const ts = entry.timestamp;
    const timeStr = [
      String(ts.getHours()).padStart(2, '0'),
      String(ts.getMinutes()).padStart(2, '0'),
      String(ts.getSeconds()).padStart(2, '0'),
    ].join(':');

    const details = document.createElement('details');
    const summary = document.createElement('summary');
    summary.textContent =
      '#' +
      i +
      ' ' +
      timeStr +
      ' \u2014 ' +
      msgCount +
      ' messages, ' +
      toolCount +
      ' tool' +
      (toolCount !== 1 ? 's' : '');
    details.appendChild(summary);
    const pre = document.createElement('pre');
    pre.className = 'request-json';
    pre.textContent = parsed ? JSON.stringify(parsed, null, 2) : entry.data;
    details.appendChild(pre);
    requestLogEl.appendChild(details);
  }
}
