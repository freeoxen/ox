// ============================================================
// ox — Brand Book Renderer
// Parses BRAND_BOOK.md and produces brand.html with rich
// visual specimens that demonstrate the design system.
// ============================================================

import { themeNames, themeByName } from "../../crates/ox-web/ui/src/theme";

// --- Palette constants ---

interface PaletteColor {
  name: string;
  hex: string;
  cssVar: string;
  initial: string;
  description: string;
}

const PALETTE: PaletteColor[] = [
  { name: "Deep Indigo", hex: "#2B2D7C", cssVar: "--indigo", initial: "I", description: "The bedrock. Backgrounds, deep surfaces, text, receding layers." },
  { name: "Slate", hex: "#6B6D8F", cssVar: "--slate", initial: "S", description: "The ox's shadow on worn stone. Cool mid-tone for muted text." },
  { name: "Vermillion", hex: "#E8471B", cssVar: "--vermillion", initial: "V", description: "The brand on the flank. Errors, emphasis. Rare because it means something." },
  { name: "Clay", hex: "#B8926E", cssVar: "--clay", initial: "C", description: "Dry earth, the path under hooves. Warm mid-tone for muted text." },
  { name: "Warm Amber", hex: "#F5A623", cssVar: "--amber", initial: "A", description: "The sun overhead. Interactive elements, highlights, active states." },
  { name: "Warm Beige", hex: "#E8D5C0", cssVar: "--beige", initial: "B", description: "Dry earth. Page background, subtle borders, secondary surfaces." },
  { name: "White", hex: "#FFFFFF", cssVar: "--white", initial: "W", description: "Open sky. Cards, panels, input fields. Space where content breathes." },
];

const HEX_TO_INITIAL: Record<string, string> = {};
for (const c of PALETTE) HEX_TO_INITIAL[c.hex] = c.initial;

// Determine a contrasting text color for a given background hex
function contrastText(bgHex: string): string {
  const hex = bgHex.replace("#", "");
  const r = parseInt(hex.substring(0, 2), 16);
  const g = parseInt(hex.substring(2, 4), 16);
  const b = parseInt(hex.substring(4, 6), 16);
  const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
  return lum > 0.55 ? "#2B2D7C" : "#FFFFFF";
}

// --- Section parser ---

interface Section {
  number: string; // "01", "02", ... or "" for preamble
  title: string;
  raw: string;
}

function parseSections(md: string): Section[] {
  const sections: Section[] = [];
  const lines = md.split("\n");
  let current: Section | null = null;
  let buf: string[] = [];

  for (const line of lines) {
    const m = line.match(/^## (\d+) — (.+)$/);
    if (m) {
      if (current) {
        current.raw = buf.join("\n");
        sections.push(current);
      } else if (buf.length > 0) {
        // Preamble (before first ##)
        sections.push({ number: "", title: "", raw: buf.join("\n") });
      }
      current = { number: m[1], title: m[2], raw: "" };
      buf = [];
    } else {
      buf.push(line);
    }
  }
  if (current) {
    current.raw = buf.join("\n");
    sections.push(current);
  } else if (buf.length > 0) {
    sections.push({ number: "", title: "", raw: buf.join("\n") });
  }

  return sections;
}

// --- Sub-parsers ---

interface MdTable {
  headers: string[];
  rows: string[][];
}

function parseTables(raw: string): MdTable[] {
  const tables: MdTable[] = [];
  const lines = raw.split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.includes("|") && !line.startsWith("```")) {
      // Potential table header
      const headerCells = splitTableRow(line);
      if (headerCells.length > 1 && i + 1 < lines.length && lines[i + 1].match(/^\|[\s\-:|]+\|/)) {
        // Skip separator
        i += 2;
        const rows: string[][] = [];
        while (i < lines.length && lines[i].includes("|") && !lines[i].startsWith("```")) {
          const cells = splitTableRow(lines[i]);
          if (cells.length > 1) rows.push(cells);
          else break;
          i++;
        }
        tables.push({ headers: headerCells, rows });
        continue;
      }
    }
    i++;
  }
  return tables;
}

function splitTableRow(line: string): string[] {
  // Handle backtick-escaped pipes by temporarily replacing them
  let s = line.trim();
  if (s.startsWith("|")) s = s.substring(1);
  if (s.endsWith("|")) s = s.substring(0, s.length - 1);

  const cells: string[] = [];
  let cell = "";
  let inCode = false;
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "`") {
      inCode = !inCode;
      cell += s[i];
    } else if (s[i] === "|" && !inCode) {
      cells.push(cell.trim());
      cell = "";
    } else {
      cell += s[i];
    }
  }
  cells.push(cell.trim());
  return cells;
}

function parseCodeBlocks(raw: string): string[] {
  const blocks: string[] = [];
  const re = /```(\w*)\n([\s\S]*?)```/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw)) !== null) {
    blocks.push(m[2]);
  }
  return blocks;
}

function parseSubheadings(raw: string): { heading: string; content: string }[] {
  const result: { heading: string; content: string }[] = [];
  const parts = raw.split(/^### /m);
  for (let i = 1; i < parts.length; i++) {
    const nlIdx = parts[i].indexOf("\n");
    const heading = nlIdx >= 0 ? parts[i].substring(0, nlIdx).trim() : parts[i].trim();
    const content = nlIdx >= 0 ? parts[i].substring(nlIdx + 1).trim() : "";
    result.push({ heading, content });
  }
  return result;
}

function extractProse(raw: string): string[] {
  const lines = raw.split("\n");
  const paragraphs: string[] = [];
  let buf: string[] = [];
  let inCode = false;
  let inTable = false;

  for (const line of lines) {
    if (line.startsWith("```")) { inCode = !inCode; continue; }
    if (inCode) continue;
    if (line.match(/^\|.*\|/) || line.match(/^###/) || line.match(/^>/)) {
      if (buf.length > 0) { paragraphs.push(buf.join(" ")); buf = []; }
      inTable = line.includes("|");
      continue;
    }
    if (inTable && !line.includes("|")) inTable = false;
    if (inTable) continue;

    const trimmed = line.trim();
    if (trimmed === "" || trimmed === "---") {
      if (buf.length > 0) { paragraphs.push(buf.join(" ")); buf = []; }
    } else if (!trimmed.startsWith("- ") && !trimmed.startsWith("**") && !trimmed.startsWith("#")) {
      buf.push(trimmed);
    }
  }
  if (buf.length > 0) paragraphs.push(buf.join(" "));
  return paragraphs.filter((p) => p.length > 20);
}

function parseBullets(raw: string): string[] {
  const bullets: string[] = [];
  for (const line of raw.split("\n")) {
    const m = line.match(/^- (.+)$/);
    if (m) bullets.push(m[1].trim());
  }
  return bullets;
}

// --- Inline markdown formatter ---

function fmt(s: string): string {
  let out = s;
  out = out.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");
  out = out.replace(/\*(.+?)\*/g, "<em>$1</em>");
  out = out.replace(/`([^`]+)`/g, '<code>$1</code>');
  out = out.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');
  out = out.replace(/&mdash;/g, "&mdash;");
  out = out.replace(/&ndash;/g, "&ndash;");
  out = out.replace(/&harr;/g, "&harr;");
  out = out.replace(/&middot;/g, "&middot;");
  return out;
}

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

// --- Section Renderers ---

function renderPreamble(raw: string): string {
  const lines = raw.split("\n").filter((l) => l.trim() !== "" && !l.startsWith("#") && !l.startsWith("**v") && l.trim() !== "---");
  const version = raw.match(/\*\*(.+?)\*\*/)?.[1] ?? "";
  const prose = lines.join(" ").trim();

  return `
  <section class="hero hero-compact">
    <div class="hero-brand">ox <span class="hero-brand-sub">brand book</span></div>
    <p class="hero-lead">${fmt(prose)}</p>
    <div class="hero-links">
      <span class="arch-badge">${esc(version)}</span>
      <a class="hero-link hero-link-muted" href="/">Home</a>
      <a class="hero-link" href="https://github.com/freeoxen/ox">GitHub</a>
    </div>
  </section>`;
}

function renderSection01(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">01 &mdash; Essence</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  for (let i = 0; i < subs.length; i++) {
    const sub = subs[i];
    const prose = extractProse(sub.content);
    html += `\n      <div class="essence-block">`;
    html += `\n        <h3>${fmt(sub.heading)}</h3>`;
    for (const p of prose) {
      html += `\n        <p>${fmt(p)}</p>`;
    }
    html += `\n      </div>`;
    if (i < subs.length - 1) html += `\n      <hr class="section-stripe">`;
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection02(raw: string): string {
  const tables = parseTables(raw);
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && !l.includes("|") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">02 &mdash; Color Palette</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Large swatch grid
  html += `\n      <div class="palette-grid">`;
  for (const c of PALETTE) {
    const border = c.hex === "#FFFFFF" ? " border: 1px solid var(--t-border);" : "";
    const textColor = contrastText(c.hex);
    html += `
        <div class="palette-swatch">
          <div class="palette-swatch-color" style="background: ${c.hex};${border}">
            <span class="palette-swatch-hex" style="color: ${textColor}">${c.hex}</span>
          </div>
          <div class="palette-swatch-name">${esc(c.name)}</div>
          <div class="palette-swatch-var"><code>${esc(c.cssVar)}</code></div>
          <div class="palette-swatch-desc">${esc(c.description)}</div>
        </div>`;
  }
  html += `\n      </div>`;

  // Functional aliases
  if (codeBlocks.length > 0) {
    const aliasSub = subs.find((s) => s.heading.includes("Functional"));
    if (aliasSub) {
      html += `\n      <h3>Functional Aliases</h3>`;
      html += `\n      <pre class="code-block">${syntaxHighlightCss(codeBlocks[0])}</pre>`;
    }
  }

  // Color Rules
  const rulesSub = subs.find((s) => s.heading.includes("Color Rules"));
  if (rulesSub) {
    const bullets = parseBullets(rulesSub.content);
    html += `\n      <h3>Color Rules</h3>`;
    html += `\n      <ul class="brand-list">`;
    for (const b of bullets) html += `\n        <li>${fmt(b)}</li>`;
    html += `\n      </ul>`;
  }

  // Contrast Rules
  const contrastSub = subs.find((s) => s.heading.includes("Contrast Rules"));
  if (contrastSub) {
    const contrastTable = parseTables(contrastSub.content);
    if (contrastTable.length > 0) {
      html += `\n      <h3>Contrast Rules</h3>`;
      html += `\n      <div class="section-body"><p>With seven opaque colors, only certain pairings produce readable text. These rules are absolute.</p></div>`;
      html += renderContrastTable(contrastTable[0]);
    }
  }

  // Theme chips
  html += `\n      <h3>The Twelve Themes</h3>`;
  html += `\n      <div class="theme-chip-grid">`;
  for (const name of themeNames()) {
    html += `\n        <button class="theme-chip" data-theme="${esc(name)}">${esc(name)}</button>`;
  }
  html += `\n      </div>`;

  // Token matrix
  html += `\n    </div>\n  </section>`;
  html += renderTokenMatrix();

  // Narrative arc
  const narrativeSub = subs.find((s) => s.heading.includes("Narrative"));
  if (narrativeSub) {
    const prose = extractProse(narrativeSub.content);
    html += `
  <section class="site-section">
    <div class="site-inner animate-in">
      <div class="section-body">`;
    for (const p of prose) html += `\n        <p>${fmt(p)}</p>`;
    html += `\n      </div>\n    </div>\n  </section>`;
  }

  return html;
}

function renderContrastTable(table: MdTable): string {
  const pairColorMap: Record<string, string> = {
    I: "#2B2D7C", S: "#6B6D8F", V: "#E8471B", C: "#B8926E",
    A: "#F5A623", B: "#E8D5C0", W: "#FFFFFF",
  };

  // Split into two columns
  const mid = Math.ceil(table.rows.length / 2);
  const left = table.rows.slice(0, mid);
  const right = table.rows.slice(mid);

  function renderHalf(rows: string[][]): string {
    let h = `<table class="arch-table contrast-table">
          <thead><tr><th>Pairing</th><th>Ratio</th><th>Demo</th><th></th></tr></thead>
          <tbody>`;
    for (const row of rows) {
      const pairing = row[0]?.trim() ?? "";
      const ratio = row[1]?.trim() ?? "";
      const verdict = row[2]?.trim() ?? "";

      // Parse pairing like "I on W"
      const pm = pairing.match(/^(\w) on (\w)$/);
      let demo = "";
      if (pm) {
        const fg = pairColorMap[pm[1]] ?? "#000";
        const bg = pairColorMap[pm[2]] ?? "#FFF";
        demo = `<span class="contrast-demo" style="background:${bg};color:${fg}">Aa</span>`;
      }

      let verdictClass = "verdict-ok";
      const vl = verdict.toLowerCase();
      if (vl.includes("excellent") || vl.includes("good")) verdictClass = "verdict-good";
      else if (vl.includes("never")) verdictClass = "verdict-never";
      else if (vl.includes("borderline") || vl.includes("accent") || vl.includes("ui") || vl.includes("faint")) verdictClass = "verdict-warn";

      h += `\n            <tr><td class="crate-name">${esc(pairing)}</td><td>${esc(ratio)}</td><td>${demo}</td><td class="${verdictClass}">${esc(verdict.replace(/\*\*/g, ""))}</td></tr>`;
    }
    h += `\n          </tbody>\n        </table>`;
    return h;
  }

  return `
      <div class="contrast-columns">
        ${renderHalf(left)}
        ${renderHalf(right)}
      </div>`;
}

function renderTokenMatrix(): string {
  const names = themeNames();
  const tokenKeys = [
    "bg", "surface", "inset", "field",
    "onBg", "onSurface", "onInset", "onAccent",
    "heading", "accent", "danger",
    "border", "borderStrong", "stripe",
    "btnBg", "btnText", "muted", "faint",
  ];
  const shortLabels = [
    "bg", "srf", "ins", "fld",
    "onBg", "onSrf", "onIns", "onAcc",
    "head", "acc", "dng",
    "bdr", "bdrS", "strp",
    "btn", "btnT", "mut", "fnt",
  ];

  let html = `
  <section class="site-section">
    <div class="site-inner-wide animate-in">
      <h2 class="section-title">Token Matrix</h2>
      <div class="section-body">
        <p>Each cell shows the actual color assigned to that theme&times;token pair.
        <strong>I</strong>=indigo <strong>S</strong>=slate <strong>V</strong>=vermillion
        <strong>C</strong>=clay <strong>A</strong>=amber <strong>B</strong>=beige <strong>W</strong>=white.</p>
      </div>
      <div class="token-table-wrap">
        <table class="token-matrix">
          <thead><tr><th>#</th><th>Name</th>`;

  for (const lbl of shortLabels) html += `<th>${esc(lbl)}</th>`;
  html += `</tr></thead>\n          <tbody>`;

  for (let i = 0; i < names.length; i++) {
    const name = names[i];
    const def = themeByName(name);
    if (!def) continue;
    html += `\n            <tr><td>${i}</td><td>${esc(name)}</td>`;
    for (const key of tokenKeys) {
      const hex = (def.tokens as Record<string, string>)[key] ?? "#000";
      const initial = HEX_TO_INITIAL[hex] ?? "?";
      const txtColor = contrastText(hex);
      html += `<td><span class="token-cell" style="background:${hex};color:${txtColor}">${initial}</span></td>`;
    }
    html += `</tr>`;
  }

  html += `\n          </tbody>\n        </table>\n      </div>\n    </div>\n  </section>`;
  return html;
}

function renderSection03(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">03 &mdash; Typography</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Type specimens
  html += `\n      <div class="type-specimens">`;

  const fonts: { heading: string; cssClass: string; badge: string; sub: (typeof subs)[0] | undefined }[] = [
    { heading: "Manrope", cssClass: "type-sample-display", badge: "display", sub: subs.find((s) => s.heading.includes("Manrope")) },
    { heading: "Outfit", cssClass: "type-sample-body", badge: "body", sub: subs.find((s) => s.heading.includes("Outfit")) },
    { heading: "IBM Plex Mono", cssClass: "type-sample-mono", badge: "code", sub: subs.find((s) => s.heading.includes("IBM Plex Mono")) },
  ];

  for (const f of fonts) {
    const sub = f.sub;
    const tables = sub ? parseTables(sub.content) : [];
    const prose = sub ? extractProse(sub.content) : [];
    const description = prose[0] ?? "";

    html += `
        <div class="type-specimen">
          <div class="type-sample ${f.cssClass}">${esc(f.heading)}</div>
          <div class="type-meta">
            <span class="arch-badge">${f.badge}</span>
            <span class="type-detail">${fmt(description)}</span>
          </div>`;

    // Weight strip
    if (tables.length > 0) {
      html += `\n          <div class="weight-strip">`;
      for (const row of tables[0].rows) {
        const weight = row[1]?.trim() ?? "400";
        const name = row[0]?.trim() ?? "";
        html += `\n            <div class="weight-strip-item" style="font-family: var(--font-${f.badge === "display" ? "display" : f.badge === "body" ? "body" : "mono"}); font-weight: ${weight};">`;
        html += `<span class="weight-strip-sample">${esc(f.heading)}</span>`;
        html += `<span class="weight-strip-label">${esc(name)} (${weight})</span></div>`;
      }
      html += `\n          </div>`;
    }

    html += `\n        </div>`;
  }
  html += `\n      </div>`;

  // Brand title pattern
  const brandSub = subs.find((s) => s.heading.includes("Brand Title"));
  if (brandSub) {
    html += `\n      <h3>Brand Title Pattern</h3>`;
    html += `\n      <div class="brand-title-demo"><span class="brand-title-ox">ox</span> <span class="brand-title-desc">playground</span></div>`;
    const brandCode = parseCodeBlocks(brandSub.content);
    if (brandCode.length > 0) {
      html += `\n      <pre class="code-block">${esc(brandCode[0].trim())}</pre>`;
    }
  }

  // Type scale demo
  const scaleSub = subs.find((s) => s.heading.includes("Type Scale"));
  if (scaleSub) {
    html += `\n      <h3>Type Scale</h3>`;
    const steps = [
      { name: "--step-0", label: "Body text", clamp: "clamp(1rem, 0.95rem + 0.25vw, 1.125rem)" },
      { name: "--step-1", label: "Lead text, subheads", clamp: "clamp(1.25rem, 1.15rem + 0.5vw, 1.5rem)" },
      { name: "--step-2", label: "Section subheads", clamp: "clamp(1.5rem, 1.3rem + 1vw, 2rem)" },
      { name: "--step-3", label: "Section titles", clamp: "clamp(2rem, 1.6rem + 2vw, 3rem)" },
      { name: "--step-4", label: "Page titles", clamp: "clamp(2.5rem, 1.8rem + 3.5vw, 4.5rem)" },
      { name: "--step-5", label: "Hero / brand display", clamp: "clamp(3rem, 2rem + 5vw, 7rem)" },
    ];
    html += `\n      <div class="type-scale-demo">`;
    for (const step of steps) {
      html += `\n        <div class="type-scale-step" style="font-size: var(${step.name});">`;
      html += `<span class="type-scale-label">${esc(step.name)}</span> ${esc(step.label)}</div>`;
    }
    html += `\n      </div>`;

    const scaleCode = parseCodeBlocks(scaleSub.content);
    if (scaleCode.length > 0) {
      html += `\n      <pre class="code-block">${syntaxHighlightCss(scaleCode[0])}</pre>`;
    }
  }

  // Font loading
  const fontLoadSub = subs.find((s) => s.heading.includes("Font Loading"));
  if (fontLoadSub) {
    const fontCode = parseCodeBlocks(fontLoadSub.content);
    if (fontCode.length > 0) {
      html += `\n      <h3>Font Loading</h3>`;
      html += `\n      <pre class="code-block">${esc(fontCode[0].trim())}</pre>`;
    }
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection04(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">04 &mdash; Illustration Style</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Core Style Preamble — blockquote
  const preambleSub = subs.find((s) => s.heading.includes("Core Style Preamble"));
  if (preambleSub) {
    const blockquote = preambleSub.content.split("\n").filter((l) => l.startsWith("> ")).map((l) => l.substring(2)).join(" ").trim();
    if (blockquote) {
      html += `\n      <h3>Core Style Preamble</h3>`;
      html += `\n      <blockquote class="brand-callout">${fmt(blockquote)}</blockquote>`;
    }
  }

  // Shape Language
  const shapeSub = subs.find((s) => s.heading.includes("Shape Language"));
  if (shapeSub) {
    const bullets = parseBullets(shapeSub.content);
    html += `\n      <h3>Shape Language</h3>`;
    html += `\n      <dl class="shape-dl">`;
    for (const b of bullets) {
      const cm = b.match(/^\*\*(.+?)\*\*[:\s]*(.+)$/);
      if (cm) {
        html += `\n        <dt>${esc(cm[1])}</dt><dd>${fmt(cm[2])}</dd>`;
      } else {
        html += `\n        <dd>${fmt(b)}</dd>`;
      }
    }
    html += `\n      </dl>`;
  }

  // Depth Method
  const depthSub = subs.find((s) => s.heading.includes("Depth"));
  if (depthSub) {
    const prose = extractProse(depthSub.content);
    html += `\n      <h3>Depth Method</h3>`;
    for (const p of prose) html += `\n      <div class="section-body"><p>${fmt(p)}</p></div>`;
  }

  // Composition Variables
  const compSub = subs.find((s) => s.heading.includes("Composition"));
  if (compSub) {
    const tables = parseTables(compSub.content);
    if (tables.length > 0) {
      html += `\n      <h3>Composition Variables</h3>`;
      html += renderArchTable(tables[0]);
    }
  }

  // Scene Prompts
  const sceneSub = subs.find((s) => s.heading.includes("Scene Prompts"));
  if (sceneSub) {
    html += `\n      <h3>Scene Prompts</h3>`;
    // Parse scenes from bold headings
    const sceneRegex = /\*\*(.+?):\*\*\n(.+?)(?=\n\n\*\*|\n\n###|$)/gs;
    let sm: RegExpExecArray | null;
    html += `\n      <div class="scene-card-grid">`;
    while ((sm = sceneRegex.exec(sceneSub.content)) !== null) {
      html += `
        <div class="scene-card">
          <div class="scene-card-name">${esc(sm[1].trim())}</div>
          <p>${fmt(sm[2].trim())}</p>
        </div>`;
    }
    html += `\n      </div>`;
  }

  // Generation Parameters
  const genSub = subs.find((s) => s.heading.includes("Generation"));
  if (genSub) {
    const tables = parseTables(genSub.content);
    if (tables.length > 0) {
      html += `\n      <h3>Generation Parameters</h3>`;
      html += renderArchTable(tables[0]);
    }
  }

  // Do / Never
  const doSub = subs.find((s) => s.heading.includes("Do / Never"));
  if (doSub) {
    html += `\n      <h3>Do / Never</h3>`;
    const lines = doSub.content.split("\n");
    const doItems: string[] = [];
    const neverItems: string[] = [];
    let mode: "do" | "never" | null = null;
    for (const line of lines) {
      if (line.startsWith("**Do:**")) { mode = "do"; continue; }
      if (line.startsWith("**Never:**")) { mode = "never"; continue; }
      const bm = line.match(/^- (.+)$/);
      if (bm && mode === "do") doItems.push(bm[1]);
      if (bm && mode === "never") neverItems.push(bm[1]);
    }
    html += `\n      <div class="do-never-grid">`;
    html += `\n        <div class="do-column"><h4 class="do-heading">Do</h4><ul>`;
    for (const item of doItems) html += `<li class="do-item">${fmt(item)}</li>`;
    html += `</ul></div>`;
    html += `\n        <div class="never-column"><h4 class="never-heading">Never</h4><ul>`;
    for (const item of neverItems) html += `<li class="never-item">${fmt(item)}</li>`;
    html += `</ul></div>`;
    html += `\n      </div>`;
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection05(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">05 &mdash; UI Components</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Buttons
  const btnSub = subs.find((s) => s.heading === "Buttons");
  if (btnSub) {
    html += `\n      <h3>Buttons</h3>`;
    html += `\n      <div class="specimen-row">`;
    html += `\n        <div class="specimen-item"><button class="btn btn-primary">Primary</button><span class="specimen-label">Primary</span></div>`;
    html += `\n        <div class="specimen-item"><button class="btn btn-secondary">Secondary</button><span class="specimen-label">Secondary</span></div>`;
    html += `\n        <div class="specimen-item"><button class="btn btn-accent">Accent</button><span class="specimen-label">Accent</span></div>`;
    html += `\n        <div class="specimen-item"><button class="btn btn-ghost">Ghost</button><span class="specimen-label">Ghost</span></div>`;
    html += `\n      </div>`;
    const btnCode = parseCodeBlocks(btnSub.content);
    if (btnCode.length > 0) {
      html += `\n      <pre class="code-block">${syntaxHighlightCss(btnCode[0])}</pre>`;
    }
  }

  // Inputs
  const inputSub = subs.find((s) => s.heading === "Inputs");
  if (inputSub) {
    html += `\n      <h3>Inputs</h3>`;
    html += `\n      <div class="specimen-row specimen-row-inputs">`;
    html += `\n        <div class="specimen-item specimen-item-wide"><input class="ox-input" type="text" placeholder="Text input specimen"><span class="specimen-label">Text Input</span></div>`;
    html += `\n        <div class="specimen-item specimen-item-wide"><textarea class="ox-input ox-textarea" placeholder="Textarea specimen" rows="2"></textarea><span class="specimen-label">Textarea</span></div>`;
    html += `\n      </div>`;
    const inputCode = parseCodeBlocks(inputSub.content);
    if (inputCode.length > 0) {
      html += `\n      <pre class="code-block">${syntaxHighlightCss(inputCode[0])}</pre>`;
    }
  }

  // Badges
  const badgeSub = subs.find((s) => s.heading === "Badges");
  if (badgeSub) {
    html += `\n      <h3>Badges</h3>`;
    html += `\n      <div class="specimen-row">`;
    html += `\n        <div class="specimen-item"><span class="ox-badge ox-badge-indigo">Indigo</span><span class="specimen-label">Indigo</span></div>`;
    html += `\n        <div class="specimen-item"><span class="ox-badge ox-badge-amber">Amber</span><span class="specimen-label">Amber</span></div>`;
    html += `\n        <div class="specimen-item"><span class="ox-badge ox-badge-vermillion">Vermillion</span><span class="specimen-label">Vermillion</span></div>`;
    html += `\n        <div class="specimen-item"><span class="ox-badge ox-badge-ghost">Ghost</span><span class="specimen-label">Ghost</span></div>`;
    html += `\n      </div>`;
  }

  // Status Indicators
  const statusSub = subs.find((s) => s.heading.includes("Status"));
  if (statusSub) {
    html += `\n      <h3>Status Indicators</h3>`;
    html += `\n      <div class="specimen-row">`;
    html += `\n        <div class="specimen-item"><span class="status-dot status-active"></span><span class="specimen-label">Active</span></div>`;
    html += `\n        <div class="specimen-item"><span class="status-dot status-idle"></span><span class="specimen-label">Idle</span></div>`;
    html += `\n        <div class="specimen-item"><span class="status-dot status-error"></span><span class="specimen-label">Error</span></div>`;
    html += `\n      </div>`;
  }

  // Cards
  const cardSub = subs.find((s) => s.heading === "Cards");
  if (cardSub) {
    html += `\n      <h3>Cards</h3>`;
    html += `\n      <div class="specimen-card-demo">`;
    html += `\n        <div class="ox-card ox-card-amber"><div class="ox-card-tag">tool</div><div class="ox-card-body">A card with an amber stripe. The thin band of earth color identifies what the card carries.</div></div>`;
    html += `\n        <div class="ox-card ox-card-indigo"><div class="ox-card-tag">system</div><div class="ox-card-body">Indigo stripe for system and configuration items.</div></div>`;
    html += `\n        <div class="ox-card ox-card-vermillion"><div class="ox-card-tag">error</div><div class="ox-card-body">Vermillion stripe for errors and alerts.</div></div>`;
    html += `\n      </div>`;
    const cardCode = parseCodeBlocks(cardSub.content);
    if (cardCode.length > 0) {
      html += `\n      <pre class="code-block">${syntaxHighlightCss(cardCode[0])}</pre>`;
    }
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection06(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">06 &mdash; Layout</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Structure ASCII
  if (codeBlocks.length > 0) {
    html += `\n      <h3>Structure</h3>`;
    html += `\n      <pre class="code-block">${esc(codeBlocks[0].trim())}</pre>`;
  }

  // Spacing system
  const spacingSub = subs.find((s) => s.heading.includes("Spacing"));
  if (spacingSub) {
    html += `\n      <h3>Spacing System</h3>`;

    const spacingTokens = [
      { name: "--space-xs", value: "0.5rem", px: 8 },
      { name: "--space-sm", value: "1rem", px: 16 },
      { name: "--space-md", value: "2rem", px: 32 },
      { name: "--space-lg", value: "4rem", px: 64 },
      { name: "--space-xl", value: "6rem", px: 96 },
      { name: "--space-2xl", value: "10rem", px: 160 },
    ];

    // Visual spacing demo
    html += `\n      <div class="spacing-demo">`;
    const maxPx = 160;
    for (const token of spacingTokens) {
      const pct = Math.round((token.px / maxPx) * 100);
      html += `
        <div class="spacing-bar-row">
          <div class="spacing-bar" style="width: ${pct}%;"><span class="spacing-bar-label">${esc(token.name)}</span></div>
          <span class="spacing-bar-value">${token.px}px</span>
        </div>`;
    }
    html += `\n      </div>`;

    // Spacing table
    const tables = parseTables(spacingSub.content);
    if (tables.length > 0) {
      html += renderArchTable(tables[0]);
    }
  }

  // Borders
  const borderSub = subs.find((s) => s.heading.includes("Borders"));
  if (borderSub) {
    html += `\n      <h3>Borders and Dividers</h3>`;
    const bullets = parseBullets(borderSub.content);
    html += `\n      <ul class="brand-list">`;
    for (const b of bullets) html += `\n        <li>${fmt(b)}</li>`;
    html += `\n      </ul>`;
    const prose = extractProse(borderSub.content);
    for (const p of prose) {
      html += `\n      <div class="section-body"><p>${fmt(p)}</p></div>`;
    }
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection07(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">07 &mdash; Motion</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Easing Functions
  const easingSub = subs.find((s) => s.heading.includes("Easing"));
  if (easingSub) {
    const tables = parseTables(easingSub.content);
    if (tables.length > 0) {
      html += `\n      <h3>Easing Functions</h3>`;
      html += renderArchTable(tables[0]);
    }
  }

  // Easing demos
  const easings = [
    { name: "Enter", value: "cubic-bezier(0.22, 1, 0.36, 1)", duration: "1.2s", desc: "The long approach" },
    { name: "Spring", value: "cubic-bezier(0.34, 1.56, 0.64, 1)", duration: "0.8s", desc: "The head toss" },
    { name: "Pulse", value: "ease-in-out", duration: "1.2s", desc: "The heartbeat" },
  ];

  html += `\n      <div class="easing-demos">`;
  for (const e of easings) {
    html += `
        <div class="easing-demo">
          <div class="easing-label"><strong>${esc(e.name)}</strong> <code>${esc(e.value)}</code></div>
          <div class="easing-track" data-easing="${esc(e.value)}" data-duration="${esc(e.duration)}">
            <div class="easing-dot"></div>
          </div>
        </div>`;
  }
  html += `\n      </div>`;

  // Patterns
  const patternsSub = subs.find((s) => s.heading.includes("Patterns"));
  if (patternsSub) {
    html += `\n      <h3>Motion Patterns</h3>`;
    html += `\n      <div class="motion-patterns">`;
    // Enter demo
    html += `\n        <div class="motion-pattern-demo">`;
    html += `\n          <div class="motion-enter-card">Enter &mdash; rise from below with fade</div>`;
    html += `\n        </div>`;
    // Hover demo
    html += `\n        <div class="motion-pattern-demo">`;
    html += `\n          <button class="btn btn-primary motion-hover-btn">Hover me</button>`;
    html += `\n        </div>`;
    // Pulse demo
    html += `\n        <div class="motion-pattern-demo">`;
    html += `\n          <span class="status-dot status-active"></span> <span class="specimen-label">Pulse &mdash; the steady breath</span>`;
    html += `\n        </div>`;
    html += `\n      </div>`;
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection08(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw.split("###")[0].split("\n").filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---").join(" ").trim();

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">08 &mdash; Iconography</h2>`;

  if (introProse) {
    html += `\n      <div class="section-body"><p>${fmt(introProse)}</p></div>`;
  }

  // Icon Metaphors
  const metSub = subs.find((s) => s.heading.includes("Icon Metaphors"));
  if (metSub) {
    const tables = parseTables(metSub.content);
    if (tables.length > 0) {
      html += `\n      <h3>Icon Metaphors</h3>`;
      html += renderArchTable(tables[0]);
    }
  }

  // Construction Rules
  const rulesSub = subs.find((s) => s.heading.includes("Construction"));
  if (rulesSub) {
    html += `\n      <h3>Construction Rules</h3>`;
    const bullets = parseBullets(rulesSub.content);
    html += `\n      <ul class="brand-list">`;
    for (const b of bullets) html += `\n        <li>${fmt(b)}</li>`;
    html += `\n      </ul>`;
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

function renderSection09(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);

  let html = `
  <section class="site-section">
    <div class="site-inner animate-in">
      <h2 class="section-title">09 &mdash; CSS Variable Reference</h2>
      <div class="section-body"><p>The full harness. Every token the system needs, nothing it doesn't.</p></div>`;

  const subNames = ["Colors", "Typography", "Spacing", "Shape", "Motion"];
  for (let i = 0; i < subs.length && i < codeBlocks.length; i++) {
    const label = subNames[i] ?? subs[i].heading;
    html += `\n      <h3>${esc(label)}</h3>`;
    html += `\n      <pre class="code-block">${syntaxHighlightCss(codeBlocks[i])}</pre>`;
  }

  // Theme tokens list (from markdown prose at end)
  const lastProse = raw.split("###").pop() ?? "";
  if (lastProse.includes("--t-")) {
    // Already covered by code blocks above
  }

  html += `\n    </div>\n  </section>`;
  return html;
}

// --- Helper: render an arch-table from parsed MdTable ---

function renderArchTable(table: MdTable): string {
  let html = `\n      <table class="arch-table">\n        <thead><tr>`;
  for (const h of table.headers) html += `<th>${fmt(h)}</th>`;
  html += `</tr></thead>\n        <tbody>`;
  for (const row of table.rows) {
    html += `\n          <tr>`;
    for (let i = 0; i < row.length; i++) {
      const cls = i === 0 ? ' class="crate-name"' : "";
      html += `<td${cls}>${fmt(row[i])}</td>`;
    }
    html += `</tr>`;
  }
  html += `\n        </tbody>\n      </table>`;
  return html;
}

// --- Syntax highlighting for CSS code blocks ---

function syntaxHighlightCss(code: string): string {
  return esc(code.trim())
    .replace(/(\/\*.*?\*\/)/g, '<span class="code-comment">$1</span>');
}

// --- Page shell ---

function pageShell(body: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ox &mdash; Brand Book</title>
  <meta name="description" content="ox brand book. Seven colors, twelve themes, three typefaces. The design system for the ox agentic AI framework.">
  <link rel="stylesheet" href="/theme.css">
  <link rel="stylesheet" href="/site.css">
</head>
<body>

  <div id="theme-picker"></div>
${body}
  <footer class="site-footer">
    <span class="footer-brand">ox</span>
    <span class="footer-by">by AdjectiveNoun</span>
    <span class="footer-sep">&middot;</span>
    <span class="footer-closing">The ox does not explain itself. It walks. The ground remembers.</span>
  </footer>

  <script type="module" src="/main.js"></script>
</body>
</html>`;
}

// --- Main export ---

const RENDERERS: Record<string, (raw: string) => string> = {
  "01": renderSection01,
  "02": renderSection02,
  "03": renderSection03,
  "04": renderSection04,
  "05": renderSection05,
  "06": renderSection06,
  "07": renderSection07,
  "08": renderSection08,
  "09": renderSection09,
};

export function renderBrandBook(md: string): string {
  const sections = parseSections(md);
  let body = "";

  for (const section of sections) {
    if (section.number === "") {
      body += renderPreamble(section.raw);
    } else {
      const renderer = RENDERERS[section.number];
      if (renderer) {
        body += renderer(section.raw);
      }
    }
  }

  return pageShell(body);
}
