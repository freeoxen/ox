import { writable } from "svelte/store";

export const status = writable<{ label: string; detail: string }>({
  label: "",
  detail: "",
});

export function setStatus(label: string, detail: string): void {
  status.set({ label, detail });
}
