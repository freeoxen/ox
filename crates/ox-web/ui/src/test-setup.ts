import { GlobalWindow } from "happy-dom";

const window = new GlobalWindow();

// Register browser globals that our code uses
const globals = [
  "document",
  "HTMLElement",
  "HTMLDivElement",
  "HTMLInputElement",
  "HTMLTextAreaElement",
  "HTMLButtonElement",
  "HTMLDetailsElement",
  "HTMLSelectElement",
  "HTMLSpanElement",
  "HTMLPreElement",
  "Element",
  "Node",
  "localStorage",
  "sessionStorage",
  "SVGElement",
  "SVGSVGElement",
  "SVGCircleElement",
  "SVGLineElement",
  "SVGGElement",
  "SVGRectElement",
] as const;

for (const name of globals) {
  // @ts-expect-error assigning browser globals
  globalThis[name] = window[name];
}
