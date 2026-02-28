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
  {
    name: "Deep Indigo",
    hex: "#2B2D7C",
    cssVar: "--indigo",
    initial: "I",
    description:
      "The bedrock. Backgrounds, deep surfaces, text, receding layers.",
  },
  {
    name: "Slate",
    hex: "#6B6D8F",
    cssVar: "--slate",
    initial: "S",
    description: "The ox's shadow on worn stone. Cool mid-tone for muted text.",
  },
  {
    name: "Vermillion",
    hex: "#E8471B",
    cssVar: "--vermillion",
    initial: "V",
    description:
      "The brand on the flank. Errors, emphasis. Rare because it means something.",
  },
  {
    name: "Clay",
    hex: "#B8926E",
    cssVar: "--clay",
    initial: "C",
    description:
      "Dry earth, the path under hooves. Warm mid-tone for muted text.",
  },
  {
    name: "Warm Amber",
    hex: "#F5A623",
    cssVar: "--amber",
    initial: "A",
    description:
      "The sun overhead. Interactive elements, highlights, active states.",
  },
  {
    name: "Warm Beige",
    hex: "#E8D5C0",
    cssVar: "--beige",
    initial: "B",
    description:
      "Dry earth. Page background, subtle borders, secondary surfaces.",
  },
  {
    name: "White",
    hex: "#FFFFFF",
    cssVar: "--white",
    initial: "W",
    description:
      "Open sky. Cards, panels, input fields. Space where content breathes.",
  },
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
      if (
        headerCells.length > 1 &&
        i + 1 < lines.length &&
        lines[i + 1].match(/^\|[\s\-:|]+\|/)
      ) {
        // Skip separator
        i += 2;
        const rows: string[][] = [];
        while (
          i < lines.length &&
          lines[i].includes("|") &&
          !lines[i].startsWith("```")
        ) {
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
    const heading =
      nlIdx >= 0 ? parts[i].substring(0, nlIdx).trim() : parts[i].trim();
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
    if (line.startsWith("```")) {
      inCode = !inCode;
      continue;
    }
    if (inCode) continue;
    if (line.match(/^\|.*\|/) || line.match(/^###/) || line.match(/^>/)) {
      if (buf.length > 0) {
        paragraphs.push(buf.join(" "));
        buf = [];
      }
      inTable = line.includes("|");
      continue;
    }
    if (inTable && !line.includes("|")) inTable = false;
    if (inTable) continue;

    const trimmed = line.trim();
    if (trimmed === "" || trimmed === "---") {
      if (buf.length > 0) {
        paragraphs.push(buf.join(" "));
        buf = [];
      }
    } else if (
      !trimmed.startsWith("- ") &&
      !trimmed.startsWith("**") &&
      !trimmed.startsWith("#")
    ) {
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
  out = out.replace(/`([^`]+)`/g, "<code>$1</code>");
  out = out.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');
  out = out.replace(/&mdash;/g, "&mdash;");
  out = out.replace(/&ndash;/g, "&ndash;");
  out = out.replace(/&harr;/g, "&harr;");
  out = out.replace(/&middot;/g, "&middot;");
  return out;
}

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

// --- Section shell ---
// Wraps content in a section with number marker, stripe, and alternating surface.
// Even sections (02,04,06,08) get an inset background — terrain layers.

function sectionShell(
  num: string,
  title: string,
  inner: string,
  wide = false,
): string {
  const n = parseInt(num, 10);
  const inset = n % 2 === 0;
  const surfaceClass = inset ? " section-inset" : "";
  const innerClass = wide ? "site-inner-wide" : "site-inner";
  return `
  <section class="site-section${surfaceClass}">
    <div class="${innerClass} animate-in">
      <div class="section-header">
        <span class="section-num">${esc(num)}</span>
        <h2 class="section-title">${esc(title)}</h2>
      </div>
${inner}
    </div>
  </section>`;
}

// --- Section Renderers ---

function renderPreamble(raw: string): string {
  const version = raw.match(/\*\*(.+?)\*\*/)?.[1] ?? "";

  // Split into paragraphs on blank lines, filtering out heading/version/rule lines
  const paragraphs: string[] = [];
  let buf: string[] = [];
  for (const line of raw.split("\n")) {
    const t = line.trim();
    if (t === "" || t === "---") {
      if (buf.length > 0) {
        paragraphs.push(buf.join(" "));
        buf = [];
      }
    } else if (!t.startsWith("#") && !t.startsWith("**v")) {
      buf.push(t);
    }
  }
  if (buf.length > 0) paragraphs.push(buf.join(" "));

  // Last paragraph is the manifesto — short, punchy, stands alone
  const manifesto = paragraphs.length > 1 ? paragraphs.pop()! : "";
  const narrative = paragraphs;

  // Palette as a horizontal terrain band — flat, layered, no tricks
  let paletteBar = `\n    <div class="hero-terrain">`;
  for (const c of PALETTE) {
    paletteBar += `<div class="hero-terrain-band" style="background:${c.hex}"></div>`;
  }
  paletteBar += `</div>`;

  return `
  <section class="hero hero-brand-hero">
    <div class="hero-brand">ox<sup style="font-size:.4em">™</sup></div>
    <div class="hero-tagline">brand book</div>
    <p class="hero-manifesto">${fmt(manifesto)}</p>${paletteBar}
    <div class="hero-prose">
${narrative.map((p) => `      <p class="hero-lead">${fmt(p)}</p>`).join("\n")}
    </div>
    <div class="hero-links">
      <span class="arch-badge">${esc(version)}</span>
      <a class="hero-link hero-link-muted" href="/">Home</a>
      <a class="hero-link" href="https://github.com/freeoxen/ox">GitHub</a>
    </div>
  </section>`;
}

function renderSection01(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  for (let i = 0; i < subs.length; i++) {
    const sub = subs[i];
    const prose = extractProse(sub.content);
    inner += `      <div class="essence-block">\n`;
    inner += `        <h3>${fmt(sub.heading)}</h3>\n`;
    for (const p of prose) {
      inner += `        <p>${fmt(p)}</p>\n`;
    }
    inner += `      </div>\n`;
    if (i < subs.length - 1) inner += `      <hr class="section-stripe">\n`;
  }

  return sectionShell("01", "Essence", inner);
}

function renderSection02(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter(
      (l) =>
        l.trim() &&
        !l.startsWith("#") &&
        !l.includes("|") &&
        l.trim() !== "---",
    )
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  // Large swatch grid
  inner += `      <div class="palette-grid">\n`;
  for (const c of PALETTE) {
    const border =
      c.hex === "#FFFFFF" ? " border: 1px solid var(--t-border);" : "";
    const textColor = contrastText(c.hex);
    inner += `        <div class="palette-swatch">
          <div class="palette-swatch-color" style="background: ${c.hex};${border}">
            <span class="palette-swatch-hex" style="color: ${textColor}">${c.hex}</span>
          </div>
          <div class="palette-swatch-name">${esc(c.name)}</div>
          <div class="palette-swatch-var"><code>${esc(c.cssVar)}</code></div>
          <div class="palette-swatch-desc">${esc(c.description)}</div>
        </div>\n`;
  }
  inner += `      </div>\n`;

  // Functional aliases
  if (codeBlocks.length > 0) {
    const aliasSub = subs.find((s) => s.heading.includes("Functional"));
    if (aliasSub) {
      inner += `      <h3>Functional Aliases</h3>\n`;
      inner += `      <pre class="code-block">${syntaxHighlightCss(codeBlocks[0])}</pre>\n`;
    }
  }

  // Color Rules
  const rulesSub = subs.find((s) => s.heading.includes("Color Rules"));
  if (rulesSub) {
    const bullets = parseBullets(rulesSub.content);
    inner += `      <h3>Color Rules</h3>\n`;
    inner += `      <ul class="brand-list">\n`;
    for (const b of bullets) inner += `        <li>${fmt(b)}</li>\n`;
    inner += `      </ul>\n`;
  }

  // Contrast Rules
  const contrastSub = subs.find((s) => s.heading.includes("Contrast Rules"));
  if (contrastSub) {
    const contrastTable = parseTables(contrastSub.content);
    if (contrastTable.length > 0) {
      inner += `      <h3>Contrast Rules</h3>\n`;
      inner += `      <div class="section-body"><p>With seven opaque colors, only certain pairings produce readable text. These rules are absolute.</p></div>\n`;
      inner += renderContrastTable(contrastTable[0]) + "\n";
    }
  }

  // Theme chips
  inner += `      <h3>The Twelve Themes</h3>\n`;
  inner += `      <div class="theme-chip-grid">\n`;
  for (const name of themeNames()) {
    inner += `        <button class="theme-chip" data-theme="${esc(name)}">${esc(name)}</button>\n`;
  }
  inner += `      </div>\n`;

  let html = sectionShell("02", "Color Palette", inner);

  // Token matrix — gets its own wide section
  html += renderTokenMatrix();

  // Narrative arc
  const narrativeSub = subs.find((s) => s.heading.includes("Narrative"));
  if (narrativeSub) {
    const prose = extractProse(narrativeSub.content);
    let narInner = `      <div class="section-body">\n`;
    for (const p of prose) narInner += `        <p>${fmt(p)}</p>\n`;
    narInner += `      </div>\n`;
    html += `
  <section class="site-section">
    <div class="site-inner animate-in">
${narInner}    </div>
  </section>`;
  }

  return html;
}

function renderContrastTable(table: MdTable): string {
  const pairColorMap: Record<string, string> = {
    I: "#2B2D7C",
    S: "#6B6D8F",
    V: "#E8471B",
    C: "#B8926E",
    A: "#F5A623",
    B: "#E8D5C0",
    W: "#FFFFFF",
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
      if (vl.includes("excellent") || vl.includes("good"))
        verdictClass = "verdict-good";
      else if (vl.includes("never")) verdictClass = "verdict-never";
      else if (
        vl.includes("borderline") ||
        vl.includes("accent") ||
        vl.includes("ui") ||
        vl.includes("faint")
      )
        verdictClass = "verdict-warn";

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
    "bg",
    "surface",
    "inset",
    "field",
    "onBg",
    "onSurface",
    "onInset",
    "onAccent",
    "heading",
    "accent",
    "danger",
    "border",
    "borderStrong",
    "stripe",
    "btnBg",
    "btnText",
    "muted",
    "faint",
  ];
  const shortLabels = [
    "bg",
    "srf",
    "ins",
    "fld",
    "onBg",
    "onSrf",
    "onIns",
    "onAcc",
    "head",
    "acc",
    "dng",
    "bdr",
    "bdrS",
    "strp",
    "btn",
    "btnT",
    "mut",
    "fnt",
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
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  // Type specimens
  inner += `      <div class="type-specimens">\n`;

  const fonts: {
    heading: string;
    cssClass: string;
    badge: string;
    sub: (typeof subs)[0] | undefined;
  }[] = [
    {
      heading: "Manrope",
      cssClass: "type-sample-display",
      badge: "display",
      sub: subs.find((s) => s.heading.includes("Manrope")),
    },
    {
      heading: "Outfit",
      cssClass: "type-sample-body",
      badge: "body",
      sub: subs.find((s) => s.heading.includes("Outfit")),
    },
    {
      heading: "IBM Plex Mono",
      cssClass: "type-sample-mono",
      badge: "code",
      sub: subs.find((s) => s.heading.includes("IBM Plex Mono")),
    },
  ];

  for (const f of fonts) {
    const sub = f.sub;
    const tables = sub ? parseTables(sub.content) : [];
    const prose = sub ? extractProse(sub.content) : [];
    const description = prose[0] ?? "";

    inner += `        <div class="type-specimen">
          <div class="type-sample ${f.cssClass}">${esc(f.heading)}</div>
          <div class="type-meta">
            <span class="arch-badge">${f.badge}</span>
            <span class="type-detail">${fmt(description)}</span>
          </div>\n`;

    if (tables.length > 0) {
      inner += `          <div class="weight-strip">\n`;
      for (const row of tables[0].rows) {
        const weight = row[1]?.trim() ?? "400";
        const name = row[0]?.trim() ?? "";
        inner += `            <div class="weight-strip-item" style="font-family: var(--font-${f.badge === "display" ? "display" : f.badge === "body" ? "body" : "mono"}); font-weight: ${weight};"><span class="weight-strip-sample">${esc(f.heading)}</span><span class="weight-strip-label">${esc(name)} (${weight})</span></div>\n`;
      }
      inner += `          </div>\n`;
    }

    inner += `        </div>\n`;
  }
  inner += `      </div>\n`;

  const brandSub = subs.find((s) => s.heading.includes("Brand Title"));
  if (brandSub) {
    inner += `      <h3>Brand Title Pattern</h3>\n`;
    inner += `      <div class="brand-title-demo"><span class="brand-title-ox">ox<sup style="font-size:.4em">™</sup></span> <span class="brand-title-desc">playground</span></div>\n`;
    const brandCode = parseCodeBlocks(brandSub.content);
    if (brandCode.length > 0) {
      inner += `      <pre class="code-block">${esc(brandCode[0].trim())}</pre>\n`;
    }
  }

  const scaleSub = subs.find((s) => s.heading.includes("Type Scale"));
  if (scaleSub) {
    inner += `      <h3>Type Scale</h3>\n`;
    const steps = [
      { name: "--step-0", label: "Body text" },
      { name: "--step-1", label: "Lead text, subheads" },
      { name: "--step-2", label: "Section subheads" },
      { name: "--step-3", label: "Section titles" },
      { name: "--step-4", label: "Page titles" },
      { name: "--step-5", label: "Hero / brand display" },
    ];
    inner += `      <div class="type-scale-demo">\n`;
    for (const step of steps) {
      inner += `        <div class="type-scale-step" style="font-size: var(${step.name});"><span class="type-scale-label">${esc(step.name)}</span> ${esc(step.label)}</div>\n`;
    }
    inner += `      </div>\n`;
    const scaleCode = parseCodeBlocks(scaleSub.content);
    if (scaleCode.length > 0) {
      inner += `      <pre class="code-block">${syntaxHighlightCss(scaleCode[0])}</pre>\n`;
    }
  }

  const fontLoadSub = subs.find((s) => s.heading.includes("Font Loading"));
  if (fontLoadSub) {
    const fontCode = parseCodeBlocks(fontLoadSub.content);
    if (fontCode.length > 0) {
      inner += `      <h3>Font Loading</h3>\n`;
      inner += `      <pre class="code-block">${esc(fontCode[0].trim())}</pre>\n`;
    }
  }

  return sectionShell("03", "Typography", inner);
}

function renderSection04(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  const preambleSub = subs.find((s) =>
    s.heading.includes("Core Style Preamble"),
  );
  if (preambleSub) {
    const blockquote = preambleSub.content
      .split("\n")
      .filter((l) => l.startsWith("> "))
      .map((l) => l.substring(2))
      .join(" ")
      .trim();
    if (blockquote) {
      inner += `      <h3>Core Style Preamble</h3>\n`;
      inner += `      <blockquote class="brand-callout">${fmt(blockquote)}</blockquote>\n`;
    }
  }

  const shapeSub = subs.find((s) => s.heading.includes("Shape Language"));
  if (shapeSub) {
    const bullets = parseBullets(shapeSub.content);
    inner += `      <h3>Shape Language</h3>\n      <dl class="shape-dl">\n`;
    for (const b of bullets) {
      const cm = b.match(/^\*\*(.+?)\*\*[:\s]*(.+)$/);
      if (cm) inner += `        <dt>${esc(cm[1])}</dt><dd>${fmt(cm[2])}</dd>\n`;
      else inner += `        <dd>${fmt(b)}</dd>\n`;
    }
    inner += `      </dl>\n`;
  }

  const depthSub = subs.find((s) => s.heading.includes("Depth"));
  if (depthSub) {
    const prose = extractProse(depthSub.content);
    inner += `      <h3>Depth Method</h3>\n`;
    for (const p of prose)
      inner += `      <div class="section-body"><p>${fmt(p)}</p></div>\n`;
  }

  const compSub = subs.find((s) => s.heading.includes("Composition"));
  if (compSub) {
    const tables = parseTables(compSub.content);
    if (tables.length > 0) {
      inner += `      <h3>Composition Variables</h3>\n`;
      inner += renderArchTable(tables[0]) + "\n";
    }
  }

  const sceneSub = subs.find((s) => s.heading.includes("Scene Prompts"));
  if (sceneSub) {
    inner += `      <h3>Scene Prompts</h3>\n      <div class="scene-card-grid">\n`;
    const sceneRegex = /\*\*(.+?):\*\*\n(.+?)(?=\n\n\*\*|\n\n###|$)/gs;
    let sm: RegExpExecArray | null;
    while ((sm = sceneRegex.exec(sceneSub.content)) !== null) {
      inner += `        <div class="scene-card"><div class="scene-card-name">${esc(sm[1].trim())}</div><p>${fmt(sm[2].trim())}</p></div>\n`;
    }
    inner += `      </div>\n`;
  }

  const genSub = subs.find((s) => s.heading.includes("Generation"));
  if (genSub) {
    const tables = parseTables(genSub.content);
    if (tables.length > 0) {
      inner += `      <h3>Generation Parameters</h3>\n`;
      inner += renderArchTable(tables[0]) + "\n";
    }
  }

  const doSub = subs.find((s) => s.heading.includes("Do / Never"));
  if (doSub) {
    inner += `      <h3>Do / Never</h3>\n`;
    const lines = doSub.content.split("\n");
    const doItems: string[] = [];
    const neverItems: string[] = [];
    let mode: "do" | "never" | null = null;
    for (const line of lines) {
      if (line.startsWith("**Do:**")) {
        mode = "do";
        continue;
      }
      if (line.startsWith("**Never:**")) {
        mode = "never";
        continue;
      }
      const bm = line.match(/^- (.+)$/);
      if (bm && mode === "do") doItems.push(bm[1]);
      if (bm && mode === "never") neverItems.push(bm[1]);
    }
    inner += `      <div class="do-never-grid">\n`;
    inner += `        <div class="do-column"><h4 class="do-heading">Do</h4><ul>`;
    for (const item of doItems)
      inner += `<li class="do-item">${fmt(item)}</li>`;
    inner += `</ul></div>\n`;
    inner += `        <div class="never-column"><h4 class="never-heading">Never</h4><ul>`;
    for (const item of neverItems)
      inner += `<li class="never-item">${fmt(item)}</li>`;
    inner += `</ul></div>\n      </div>\n`;
  }

  return sectionShell("04", "Illustration Style", inner);
}

function renderSection05(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  const btnSub = subs.find((s) => s.heading === "Buttons");
  if (btnSub) {
    inner += `      <h3>Buttons</h3>\n      <div class="specimen-row">\n`;
    inner += `        <div class="specimen-item"><button class="btn btn-primary">Primary</button><span class="specimen-label">Primary</span></div>\n`;
    inner += `        <div class="specimen-item"><button class="btn btn-secondary">Secondary</button><span class="specimen-label">Secondary</span></div>\n`;
    inner += `        <div class="specimen-item"><button class="btn btn-accent">Accent</button><span class="specimen-label">Accent</span></div>\n`;
    inner += `        <div class="specimen-item"><button class="btn btn-ghost">Ghost</button><span class="specimen-label">Ghost</span></div>\n`;
    inner += `      </div>\n`;
    const btnCode = parseCodeBlocks(btnSub.content);
    if (btnCode.length > 0)
      inner += `      <pre class="code-block">${syntaxHighlightCss(btnCode[0])}</pre>\n`;
  }

  const inputSub = subs.find((s) => s.heading === "Inputs");
  if (inputSub) {
    inner += `      <h3>Inputs</h3>\n      <div class="specimen-row specimen-row-inputs">\n`;
    inner += `        <div class="specimen-item specimen-item-wide"><input class="ox-input" type="text" placeholder="Text input specimen"><span class="specimen-label">Text Input</span></div>\n`;
    inner += `        <div class="specimen-item specimen-item-wide"><textarea class="ox-input ox-textarea" placeholder="Textarea specimen" rows="2"></textarea><span class="specimen-label">Textarea</span></div>\n`;
    inner += `      </div>\n`;
    const inputCode = parseCodeBlocks(inputSub.content);
    if (inputCode.length > 0)
      inner += `      <pre class="code-block">${syntaxHighlightCss(inputCode[0])}</pre>\n`;
  }

  const badgeSub = subs.find((s) => s.heading === "Badges");
  if (badgeSub) {
    inner += `      <h3>Badges</h3>\n      <div class="specimen-row">\n`;
    inner += `        <div class="specimen-item"><span class="ox-badge ox-badge-indigo">Indigo</span><span class="specimen-label">Indigo</span></div>\n`;
    inner += `        <div class="specimen-item"><span class="ox-badge ox-badge-amber">Amber</span><span class="specimen-label">Amber</span></div>\n`;
    inner += `        <div class="specimen-item"><span class="ox-badge ox-badge-vermillion">Vermillion</span><span class="specimen-label">Vermillion</span></div>\n`;
    inner += `        <div class="specimen-item"><span class="ox-badge ox-badge-ghost">Ghost</span><span class="specimen-label">Ghost</span></div>\n`;
    inner += `      </div>\n`;
  }

  const statusSub = subs.find((s) => s.heading.includes("Status"));
  if (statusSub) {
    inner += `      <h3>Status Indicators</h3>\n      <div class="specimen-row">\n`;
    inner += `        <div class="specimen-item"><span class="status-dot status-active"></span><span class="specimen-label">Active</span></div>\n`;
    inner += `        <div class="specimen-item"><span class="status-dot status-idle"></span><span class="specimen-label">Idle</span></div>\n`;
    inner += `        <div class="specimen-item"><span class="status-dot status-error"></span><span class="specimen-label">Error</span></div>\n`;
    inner += `      </div>\n`;
  }

  const cardSub = subs.find((s) => s.heading === "Cards");
  if (cardSub) {
    inner += `      <h3>Cards</h3>\n      <div class="specimen-card-demo">\n`;
    inner += `        <div class="ox-card ox-card-amber"><div class="ox-card-tag">tool</div><div class="ox-card-body">A card with an amber stripe. The thin band of earth color identifies what the card carries.</div></div>\n`;
    inner += `        <div class="ox-card ox-card-indigo"><div class="ox-card-tag">system</div><div class="ox-card-body">Indigo stripe for system and configuration items.</div></div>\n`;
    inner += `        <div class="ox-card ox-card-vermillion"><div class="ox-card-tag">error</div><div class="ox-card-body">Vermillion stripe for errors and alerts.</div></div>\n`;
    inner += `      </div>\n`;
    const cardCode = parseCodeBlocks(cardSub.content);
    if (cardCode.length > 0)
      inner += `      <pre class="code-block">${syntaxHighlightCss(cardCode[0])}</pre>\n`;
  }

  return sectionShell("05", "UI Components", inner);
}

function renderSection06(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  if (codeBlocks.length > 0) {
    inner += `      <h3>Structure</h3>\n`;
    inner += `      <pre class="code-block">${esc(codeBlocks[0].trim())}</pre>\n`;
  }

  const spacingSub = subs.find((s) => s.heading.includes("Spacing"));
  if (spacingSub) {
    inner += `      <h3>Spacing System</h3>\n`;
    const spacingTokens = [
      { name: "--space-xs", px: 8 },
      { name: "--space-sm", px: 16 },
      { name: "--space-md", px: 32 },
      { name: "--space-lg", px: 64 },
      { name: "--space-xl", px: 96 },
      { name: "--space-2xl", px: 160 },
    ];
    inner += `      <div class="spacing-demo">\n`;
    for (const token of spacingTokens) {
      const pct = Math.round((token.px / 160) * 100);
      inner += `        <div class="spacing-bar-row"><div class="spacing-bar" style="width: ${pct}%;"><span class="spacing-bar-label">${esc(token.name)}</span></div><span class="spacing-bar-value">${token.px}px</span></div>\n`;
    }
    inner += `      </div>\n`;
    const tables = parseTables(spacingSub.content);
    if (tables.length > 0) inner += renderArchTable(tables[0]) + "\n";
  }

  const borderSub = subs.find((s) => s.heading.includes("Borders"));
  if (borderSub) {
    inner += `      <h3>Borders and Dividers</h3>\n`;
    const bullets = parseBullets(borderSub.content);
    inner += `      <ul class="brand-list">\n`;
    for (const b of bullets) inner += `        <li>${fmt(b)}</li>\n`;
    inner += `      </ul>\n`;
    const prose = extractProse(borderSub.content);
    for (const p of prose)
      inner += `      <div class="section-body"><p>${fmt(p)}</p></div>\n`;
  }

  return sectionShell("06", "Layout", inner);
}

function renderSection07(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  const easingSub = subs.find((s) => s.heading.includes("Easing"));
  if (easingSub) {
    const tables = parseTables(easingSub.content);
    if (tables.length > 0) {
      inner += `      <h3>Easing Functions</h3>\n`;
      inner += renderArchTable(tables[0]) + "\n";
    }
  }

  const easings = [
    {
      name: "Enter",
      value: "cubic-bezier(0.22, 1, 0.36, 1)",
      duration: "1.2s",
    },
    {
      name: "Spring",
      value: "cubic-bezier(0.34, 1.56, 0.64, 1)",
      duration: "0.8s",
    },
    { name: "Pulse", value: "ease-in-out", duration: "1.2s" },
  ];
  inner += `      <div class="easing-demos">\n`;
  for (const e of easings) {
    inner += `        <div class="easing-demo"><div class="easing-label"><strong>${esc(e.name)}</strong> <code>${esc(e.value)}</code></div><div class="easing-track" data-easing="${esc(e.value)}" data-duration="${esc(e.duration)}"><div class="easing-dot"></div></div></div>\n`;
  }
  inner += `      </div>\n`;

  const patternsSub = subs.find((s) => s.heading.includes("Patterns"));
  if (patternsSub) {
    inner += `      <h3>Motion Patterns</h3>\n      <div class="motion-patterns">\n`;
    inner += `        <div class="motion-pattern-demo"><div class="motion-enter-card">Enter &mdash; rise from below with fade</div></div>\n`;
    inner += `        <div class="motion-pattern-demo"><button class="btn btn-primary motion-hover-btn">Hover me</button></div>\n`;
    inner += `        <div class="motion-pattern-demo"><span class="status-dot status-active"></span> <span class="specimen-label">Pulse &mdash; the steady breath</span></div>\n`;
    inner += `      </div>\n`;
  }

  return sectionShell("07", "Motion", inner);
}

function renderSection08(raw: string): string {
  const subs = parseSubheadings(raw);
  const introProse = raw
    .split("###")[0]
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#") && l.trim() !== "---")
    .join(" ")
    .trim();

  let inner = "";
  if (introProse) {
    inner += `      <div class="section-body"><p>${fmt(introProse)}</p></div>\n`;
  }

  const metSub = subs.find((s) => s.heading.includes("Icon Metaphors"));
  if (metSub) {
    const tables = parseTables(metSub.content);
    if (tables.length > 0) {
      inner += `      <h3>Icon Metaphors</h3>\n`;
      inner += renderArchTable(tables[0]) + "\n";
    }
  }

  const rulesSub = subs.find((s) => s.heading.includes("Construction"));
  if (rulesSub) {
    inner += `      <h3>Construction Rules</h3>\n`;
    const bullets = parseBullets(rulesSub.content);
    inner += `      <ul class="brand-list">\n`;
    for (const b of bullets) inner += `        <li>${fmt(b)}</li>\n`;
    inner += `      </ul>\n`;
  }

  return sectionShell("08", "Iconography", inner);
}

function renderSection09(raw: string): string {
  const subs = parseSubheadings(raw);
  const codeBlocks = parseCodeBlocks(raw);

  let inner = `      <div class="section-body"><p>The full harness. Every token the system needs, nothing it doesn't.</p></div>\n`;

  const subNames = ["Colors", "Typography", "Spacing", "Shape", "Motion"];
  for (let i = 0; i < subs.length && i < codeBlocks.length; i++) {
    const label = subNames[i] ?? subs[i].heading;
    inner += `      <h3>${esc(label)}</h3>\n`;
    inner += `      <pre class="code-block">${syntaxHighlightCss(codeBlocks[i])}</pre>\n`;
  }

  return sectionShell("09", "CSS Variable Reference", inner);
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
  return esc(code.trim()).replace(
    /(\/\*.*?\*\/)/g,
    '<span class="code-comment">$1</span>',
  );
}

// --- Page shell ---

function pageShell(body: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>ox™ &mdash; Brand Book</title>
  <meta name="description" content="ox™ brand book. Seven colors, twelve themes, three typefaces. The design system for the ox™ agentic AI framework.">
  <link rel="preload" href="/fonts/manrope-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/outfit-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/ibm-plex-mono-400-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/ibm-plex-mono-500-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="/fonts/ibm-plex-mono-600-latin.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="/theme.css">
  <link rel="stylesheet" href="/site.css">
  <script>
    (function(){
      var t=localStorage.getItem('ox:theme');if(t)document.documentElement.dataset.theme=t;
    })();
  </script>
</head>
<body>

  <div id="theme-picker"><svg viewBox="0 0 100 100" width="54" height="54" class="clock" aria-hidden="true"><circle cx="50" cy="50" r="45" class="clock-face"/><g class="clock-hand-group" style="transform:rotate(30deg);transform-origin:50px 50px"><line x1="50" y1="50" x2="50" y2="20" class="clock-hand"/></g><circle cx="50" cy="50" r="3" class="clock-center"/><circle cx="50" cy="12" r="3" class="clock-dot"/><circle cx="69" cy="17.1" r="3" class="clock-dot"/><circle cx="82.9" cy="31" r="3" class="clock-dot"/><circle cx="88" cy="50" r="3" class="clock-dot"/><circle cx="82.9" cy="69" r="3" class="clock-dot"/><circle cx="69" cy="82.9" r="3" class="clock-dot"/><circle cx="50" cy="88" r="3" class="clock-dot"/><circle cx="31" cy="82.9" r="3" class="clock-dot"/><circle cx="17.1" cy="69" r="3" class="clock-dot"/><circle cx="12" cy="50" r="3" class="clock-dot"/><circle cx="17.1" cy="31" r="3" class="clock-dot"/><circle cx="31" cy="17.1" r="3" class="clock-dot"/></svg></div>
${body}
  <footer class="site-footer">
    <span class="footer-brand">ox<sup style="font-size:.4em">™</sup></span>
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
