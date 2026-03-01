// Pure theme data and functions — no Svelte dependency.
// Importable by both the SvelteKit UI and the standalone site build.

// --- Brand palette (mirrors CSS :root) ---
const I = "#2B2D7C"; // indigo
const S = "#6B6D8F"; // slate
const V = "#E8471B"; // vermillion
const C = "#B8926E"; // clay
const A = "#F5A623"; // amber
const B = "#E8D5C0"; // beige
const W = "#FFFFFF"; // white

// --- Theme token keys ---
type TokenKey =
  | "bg"
  | "surface"
  | "inset"
  | "field"
  | "onBg"
  | "onSurface"
  | "onInset"
  | "onAccent"
  | "heading"
  | "accent"
  | "danger"
  | "border"
  | "borderStrong"
  | "stripe"
  | "btnBg"
  | "btnText"
  | "muted"
  | "faint";

export interface ThemeDef {
  hour: number;
  name: string;
  desc: string;
  tokens: Record<TokenKey, string>;
}

// --- 12 themes, indexed by hour (0 = 12 o'clock) ---
export const THEMES: ThemeDef[] = [
  {
    hour: 0,
    name: "noon",
    desc: "Stark white, vermillion burns",
    tokens: {
      bg: W,
      surface: W,
      inset: B,
      field: B,
      onBg: I,
      onSurface: I,
      onInset: I,
      onAccent: W,
      heading: V,
      accent: V,
      danger: V,
      border: S,
      borderStrong: I,
      stripe: V,
      btnBg: I,
      btnText: W,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 1,
    name: "early-afternoon",
    desc: "White canvas, amber warms in",
    tokens: {
      bg: W,
      surface: W,
      inset: B,
      field: B,
      onBg: I,
      onSurface: I,
      onInset: I,
      onAccent: I,
      heading: I,
      accent: A,
      danger: V,
      border: C,
      borderStrong: I,
      stripe: A,
      btnBg: V,
      btnText: W,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 2,
    name: "late-afternoon",
    desc: "Beige earth appears",
    tokens: {
      bg: B,
      surface: W,
      inset: B,
      field: W,
      onBg: I,
      onSurface: I,
      onInset: I,
      onAccent: I,
      heading: I,
      accent: A,
      danger: V,
      border: S,
      borderStrong: I,
      stripe: A,
      btnBg: A,
      btnText: I,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 3,
    name: "golden-hour",
    desc: "Everything amber, warmest light",
    tokens: {
      bg: B,
      surface: W,
      inset: B,
      field: W,
      onBg: I,
      onSurface: I,
      onInset: I,
      onAccent: I,
      heading: I,
      accent: A,
      danger: V,
      border: S,
      borderStrong: I,
      stripe: A,
      btnBg: V,
      btnText: W,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 4,
    name: "sunset",
    desc: "Beige sky, indigo shadows rise",
    tokens: {
      bg: B,
      surface: I,
      inset: W,
      field: W,
      onBg: I,
      onSurface: B,
      onInset: I,
      onAccent: I,
      heading: V,
      accent: A,
      danger: V,
      border: S,
      borderStrong: V,
      stripe: A,
      btnBg: V,
      btnText: W,
      muted: C,
      faint: S,
    },
  },
  {
    hour: 5,
    name: "dusk",
    desc: "Indigo canvas, white surfaces float",
    tokens: {
      bg: I,
      surface: W,
      inset: B,
      field: W,
      onBg: B,
      onSurface: I,
      onInset: I,
      onAccent: I,
      heading: A,
      accent: A,
      danger: V,
      border: S,
      borderStrong: A,
      stripe: V,
      btnBg: V,
      btnText: W,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 6,
    name: "twilight",
    desc: "Full indigo, amber the only warmth",
    tokens: {
      bg: I,
      surface: I,
      inset: W,
      field: W,
      onBg: B,
      onSurface: B,
      onInset: I,
      onAccent: I,
      heading: A,
      accent: A,
      danger: V,
      border: S,
      borderStrong: A,
      stripe: A,
      btnBg: V,
      btnText: W,
      muted: C,
      faint: S,
    },
  },
  {
    hour: 7,
    name: "evening",
    desc: "Softer borders, settling in",
    tokens: {
      bg: I,
      surface: I,
      inset: B,
      field: B,
      onBg: B,
      onSurface: B,
      onInset: I,
      onAccent: I,
      heading: A,
      accent: A,
      danger: V,
      border: S,
      borderStrong: B,
      stripe: A,
      btnBg: A,
      btnText: I,
      muted: C,
      faint: S,
    },
  },
  {
    hour: 8,
    name: "night",
    desc: "White text, beige warms the dark",
    tokens: {
      bg: I,
      surface: I,
      inset: B,
      field: B,
      onBg: W,
      onSurface: W,
      onInset: I,
      onAccent: I,
      heading: A,
      accent: A,
      danger: V,
      border: S,
      borderStrong: A,
      stripe: V,
      btnBg: V,
      btnText: W,
      muted: C,
      faint: S,
    },
  },
  {
    hour: 9,
    name: "midnight",
    desc: "The coldest hour",
    tokens: {
      bg: I,
      surface: I,
      inset: W,
      field: W,
      onBg: W,
      onSurface: W,
      onInset: I,
      onAccent: I,
      heading: W,
      accent: A,
      danger: V,
      border: S,
      borderStrong: A,
      stripe: V,
      btnBg: A,
      btnText: I,
      muted: C,
      faint: S,
    },
  },
  {
    hour: 10,
    name: "dawn",
    desc: "Beige surfaces emerge, first light",
    tokens: {
      bg: I,
      surface: B,
      inset: W,
      field: W,
      onBg: B,
      onSurface: I,
      onInset: I,
      onAccent: W,
      heading: A,
      accent: V,
      danger: V,
      border: S,
      borderStrong: A,
      stripe: A,
      btnBg: A,
      btnText: I,
      muted: S,
      faint: C,
    },
  },
  {
    hour: 11,
    name: "late-morning",
    desc: "Brightening toward noon",
    tokens: {
      bg: W,
      surface: B,
      inset: W,
      field: W,
      onBg: I,
      onSurface: I,
      onInset: I,
      onAccent: W,
      heading: I,
      accent: V,
      danger: V,
      border: S,
      borderStrong: I,
      stripe: I,
      btnBg: A,
      btnText: I,
      muted: S,
      faint: C,
    },
  },
];

// --- Migration map: old 5-theme names -> new names ---
const MIGRATION: Record<string, string> = {
  day: "late-afternoon",
  dusk: "twilight",
  "high-noon": "noon",
  canyon: "dusk",
  brand: "sunset",
};

const STORAGE_KEY = "ox:theme";
const DEFAULT_THEME = "early-afternoon";

// --- Geometry helpers ---

export function hourAngle(hour: number): number {
  return (hour % 12) * 30;
}

export function hourPosition(
  hour: number,
  cx: number,
  cy: number,
  r: number,
): { x: number; y: number } {
  const angle = ((hour % 12) * 30 - 90) * (Math.PI / 180);
  return {
    x: cx + r * Math.cos(angle),
    y: cy + r * Math.sin(angle),
  };
}

export function shortestRotation(from: number, to: number): number {
  return ((to - from + 540) % 360) - 180;
}

export function angleToHour(degrees: number): number {
  const norm = ((degrees % 360) + 360) % 360;
  return Math.round(norm / 30) % 12;
}

// --- Wall-clock mapping ---

const WALL_MAP: readonly number[] = [
  //  0h  1h  2h  3h  4h  5h  6h  7h  8h  9h 10h 11h
  9, 9, 9, 9, 10, 10, 10, 11, 11, 11, 11, 11,
  // 12h 13h 14h 15h 16h 17h 18h 19h 20h 21h 22h 23h
  0, 1, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9,
];

export function wallClockToThemeHour(systemHour: number): number {
  return WALL_MAP[((systemHour % 24) + 24) % 24];
}

// --- Lookup ---

export function themeByName(name: string): ThemeDef | undefined {
  return THEMES.find((t) => t.name === name);
}

export function themeNames(): string[] {
  return THEMES.map((t) => t.name);
}

export function migrateThemeName(raw: string | null): string {
  if (!raw) return DEFAULT_THEME;
  if (MIGRATION[raw]) return MIGRATION[raw];
  if (THEMES.some((t) => t.name === raw)) return raw;
  return DEFAULT_THEME;
}

// --- Imperative DOM helpers (for non-Svelte consumers) ---

export function applyThemeDOM(name: string): void {
  document.documentElement.dataset.theme = name;
  document.body.dataset.theme = name;
  localStorage.setItem(STORAGE_KEY, name);
}

export function loadSavedTheme(): string {
  if (typeof localStorage === "undefined") return DEFAULT_THEME;
  const raw = localStorage.getItem(STORAGE_KEY);
  return migrateThemeName(raw);
}

// --- Imperative theme picker init (for site / non-Svelte pages) ---

export function initThemePicker(): void {
  const container = document.getElementById("theme-picker");
  if (!container) return;

  const CX = 50;
  const CY = 50;
  const DOT_R_OUTER = 38;
  const DOT_R_INACTIVE = 3;
  const DOT_R_ACTIVE = 4.5;
  const HIT_SIZE = 14;
  const MODE_KEY = "ox:clock-mode";

  // Build SVG
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 100 100");
  svg.setAttribute("width", "54");
  svg.setAttribute("height", "54");
  svg.classList.add("clock");
  svg.setAttribute("role", "group");
  svg.setAttribute(
    "aria-label",
    "Theme clock — drag or click an hour to switch themes",
  );

  const face = document.createElementNS(NS, "circle");
  face.setAttribute("cx", "50");
  face.setAttribute("cy", "50");
  face.setAttribute("r", "45");
  face.classList.add("clock-face");
  svg.appendChild(face);

  const handGroup = document.createElementNS(NS, "g");
  handGroup.classList.add("clock-hand-group");
  const hand = document.createElementNS(NS, "line");
  hand.setAttribute("x1", "50");
  hand.setAttribute("y1", "50");
  hand.setAttribute("x2", "50");
  hand.setAttribute("y2", "20");
  hand.classList.add("clock-hand");
  handGroup.appendChild(hand);
  svg.appendChild(handGroup);

  const center = document.createElementNS(NS, "circle");
  center.setAttribute("cx", "50");
  center.setAttribute("cy", "50");
  center.setAttribute("r", "3");
  center.classList.add("clock-center");
  svg.appendChild(center);

  // State
  let currentAngle = 0;
  let activeHour = 0;
  let dragging = false;
  let mode: "wall" | "manual" = "manual";
  let wallInterval: ReturnType<typeof setInterval> | null = null;

  // Hour dots
  const dots: SVGCircleElement[] = [];
  for (const theme of THEMES) {
    const pos = hourPosition(theme.hour, CX, CY, DOT_R_OUTER);
    const g = document.createElementNS(NS, "g");
    g.classList.add("clock-hour");
    g.setAttribute("role", "button");
    g.setAttribute("tabindex", "0");
    g.setAttribute("aria-label", `${theme.name} — ${theme.desc}`);
    g.dataset.themeName = theme.name;
    g.dataset.themeDesc = theme.desc;

    const hitRect = document.createElementNS(NS, "rect");
    hitRect.setAttribute("x", String(pos.x - HIT_SIZE / 2));
    hitRect.setAttribute("y", String(pos.y - HIT_SIZE / 2));
    hitRect.setAttribute("width", String(HIT_SIZE));
    hitRect.setAttribute("height", String(HIT_SIZE));
    hitRect.setAttribute("fill", "transparent");
    hitRect.classList.add("clock-hit");
    g.appendChild(hitRect);

    const dot = document.createElementNS(NS, "circle");
    dot.setAttribute("cx", String(pos.x));
    dot.setAttribute("cy", String(pos.y));
    dot.setAttribute("r", String(DOT_R_INACTIVE));
    dot.classList.add("clock-dot");
    g.appendChild(dot);
    dots.push(dot);

    g.addEventListener("click", () => selectTheme(theme.name));
    g.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        selectTheme(theme.name);
      }
    });

    svg.appendChild(g);
  }

  // Replace pre-rendered SVG if any, otherwise append
  const existing = container.querySelector("svg");
  if (existing) existing.remove();
  container.prepend(svg);

  function setActive(name: string) {
    const theme = THEMES.find((t) => t.name === name);
    if (!theme) return;
    activeHour = theme.hour;
    const targetAngle = hourAngle(theme.hour);
    const delta = shortestRotation(currentAngle, targetAngle);
    currentAngle += delta;
    handGroup.style.transform = `rotate(${currentAngle}deg)`;
    handGroup.style.transformOrigin = "50px 50px";

    for (let i = 0; i < THEMES.length; i++) {
      const isActive = THEMES[i].hour === theme.hour;
      dots[i].setAttribute(
        "r",
        String(isActive ? DOT_R_ACTIVE : DOT_R_INACTIVE),
      );
      dots[i].classList.toggle("clock-dot-active", isActive);
    }
  }

  function selectTheme(name: string) {
    if (mode === "wall") switchMode("manual");
    applyThemeDOM(name);
    setActive(name);
  }

  function pointerToAngle(e: PointerEvent): number {
    const rect = svg.getBoundingClientRect();
    const svgX = ((e.clientX - rect.left) / rect.width) * 100;
    const svgY = ((e.clientY - rect.top) / rect.height) * 100;
    const dx = svgX - CX;
    const dy = -(svgY - CY);
    const rad = Math.atan2(dx, dy);
    return ((rad * 180) / Math.PI + 360) % 360;
  }

  function handleDragMove(e: PointerEvent) {
    const angle = pointerToAngle(e);
    const hour = angleToHour(angle);
    if (hour !== activeHour) {
      const theme = THEMES[hour];
      if (theme) selectTheme(theme.name);
    }
  }

  svg.addEventListener("pointerdown", (e) => {
    dragging = true;
    svg.setPointerCapture(e.pointerId);
    svg.classList.add("clock-dragging");
    handGroup.classList.add("clock-hand-no-transition");
    handleDragMove(e);
  });

  svg.addEventListener("pointermove", (e) => {
    if (dragging) handleDragMove(e);
  });

  const endDrag = (e: PointerEvent) => {
    if (!dragging) return;
    dragging = false;
    svg.releasePointerCapture(e.pointerId);
    svg.classList.remove("clock-dragging");
    handGroup.classList.remove("clock-hand-no-transition");
  };
  svg.addEventListener("pointerup", endDrag);
  svg.addEventListener("pointercancel", endDrag);

  // Wall-clock mode
  function wallName(): string {
    return THEMES[wallClockToThemeHour(new Date().getHours())].name;
  }

  function startWall() {
    const tick = () => {
      const name = wallName();
      applyThemeDOM(name);
      setActive(name);
    };
    tick();
    wallInterval = setInterval(tick, 60_000);
  }

  function stopWall() {
    if (wallInterval !== null) {
      clearInterval(wallInterval);
      wallInterval = null;
    }
  }

  function switchMode(newMode: "wall" | "manual") {
    mode = newMode;
    localStorage.setItem(MODE_KEY, mode);
    if (mode === "wall") {
      startWall();
      svg.classList.add("clock-live");
    } else {
      stopWall();
      svg.classList.remove("clock-live");
    }
  }

  // Mode toggle button
  const toggleBtn =
    container.querySelector<HTMLButtonElement>(".clock-mode-toggle");
  if (toggleBtn) {
    toggleBtn.addEventListener("click", () => {
      switchMode(mode === "wall" ? "manual" : "wall");
    });
  }

  // Init
  const savedMode = localStorage.getItem(MODE_KEY);
  mode = savedMode === "wall" ? "wall" : "manual";
  const initialTheme = mode === "wall" ? wallName() : loadSavedTheme();
  applyThemeDOM(initialTheme);
  setActive(initialTheme);
  if (mode === "wall") {
    svg.classList.add("clock-live");
    startWall();
  }
}
