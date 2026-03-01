// See https://svelte.dev/docs/kit/types#app.d.ts

declare module "/pkg/ox_web.js" {
  export function create_agent(
    system_prompt: string,
    api_key: string,
  ): import("$lib/wasm").OxAgent;
  export default function init(wasmPath: string): Promise<void>;
}

declare namespace App {}
