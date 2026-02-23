import { output } from './dom';

export function append(text: string, cls?: string): void {
  const span = document.createElement('span');
  span.className = cls || '';
  span.textContent = text;
  output.appendChild(span);
  output.scrollTop = output.scrollHeight;
}

export function appendLine(text: string, cls?: string): void {
  append(text + '\n', cls);
}
