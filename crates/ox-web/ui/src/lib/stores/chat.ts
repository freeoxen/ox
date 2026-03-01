import { writable, get } from "svelte/store";

export interface ChatEntry {
  text: string;
  cls: string;
}

export const messages = writable<ChatEntry[]>([]);
export const streamingText = writable("");

export function appendMessage(text: string, cls = ""): void {
  messages.update((m) => [...m, { text, cls }]);
}

export function appendLine(text: string, cls = ""): void {
  appendMessage(text + "\n", cls);
}

export function appendToStream(text: string): void {
  streamingText.update((t) => t + text);
}

export function flushStream(cls = "assistant-msg"): void {
  const text = get(streamingText);
  if (text) {
    appendMessage(text, cls);
  }
  streamingText.set("");
}

export function clearMessages(): void {
  messages.set([]);
  streamingText.set("");
}
