import { writable } from "svelte/store";
import type { OxAgent } from "$lib/wasm";

export const agent = writable<OxAgent | null>(null);
