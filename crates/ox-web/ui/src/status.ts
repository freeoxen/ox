import { statusBar } from "./dom";

export function setStatus(label: string, detail: string): void {
  statusBar.textContent = "";
  if (!label) return;
  const dot = document.createElement("span");
  dot.className = "status-dot";
  statusBar.appendChild(dot);
  const lbl = document.createElement("span");
  lbl.className = "status-label";
  lbl.textContent = label;
  statusBar.appendChild(lbl);
  if (detail) {
    const det = document.createElement("span");
    det.className = "status-detail";
    det.textContent = detail;
    statusBar.appendChild(det);
  }
}
