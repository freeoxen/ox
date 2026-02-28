function getEl<T extends HTMLElement>(id: string): T {
  return document.getElementById(id) as T;
}

export const output = getEl<HTMLDivElement>("output");
export const input = getEl<HTMLInputElement>("input");
export const sendBtn = getEl<HTMLButtonElement>("send");
export const statusBar = getEl<HTMLDivElement>("status-bar");
export const debuggerEl = getEl<HTMLDivElement>("debugger");
export const requestLogEl = getEl<HTMLDivElement>("request-log");
export const toolPanelEl = getEl<HTMLDivElement>("tool-panel");
