export function makeEmpty(text: string): HTMLDivElement {
  const el = document.createElement("div");
  el.className = "empty";
  el.textContent = text;
  return el;
}

export function makeSection(
  label: string,
  buildBody: () => HTMLElement,
  startOpen?: boolean,
): HTMLDetailsElement {
  const details = document.createElement("details");
  if (startOpen) details.open = true;
  const summary = document.createElement("summary");
  summary.textContent = label;
  details.appendChild(summary);
  details.appendChild(buildBody());
  return details;
}

export function makeKV(
  key: string,
  value: unknown,
  type: "str" | "num" | "",
): HTMLDivElement {
  const row = document.createElement("div");
  row.className = "kv";
  const k = document.createElement("span");
  k.className = "k";
  k.textContent = key;
  const v = document.createElement("span");
  v.className =
    "v " + (type === "str" ? "str-val" : type === "num" ? "num-val" : "");
  v.textContent = value != null ? String(value) : "null";
  row.appendChild(k);
  row.appendChild(v);
  return row;
}
