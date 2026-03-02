import { writable } from "svelte/store";

export interface UsageStats {
  requests: number;
  inputTokens: number;
  outputTokens: number;
}

export const usage = writable<UsageStats>({
  requests: 0,
  inputTokens: 0,
  outputTokens: 0,
});

export function addUsage(inputTokens: number, outputTokens: number): void {
  usage.update((s) => ({
    requests: s.requests + 1,
    inputTokens: s.inputTokens + inputTokens,
    outputTokens: s.outputTokens + outputTokens,
  }));
}

export function resetUsage(): void {
  usage.set({ requests: 0, inputTokens: 0, outputTokens: 0 });
}
