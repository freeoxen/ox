import { writable } from "svelte/store";
import type { RequestLogEntry } from "$lib/types";

export const requestLog = writable<RequestLogEntry[]>([]);

export function addRequestLogEntry(data: string): void {
  requestLog.update((entries) => [...entries, { timestamp: new Date(), data }]);
}
