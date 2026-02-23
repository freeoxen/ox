// ============================================================
// ox — Analog Clock Theme Picker
// 12 themes, one per hour. SVG clock face. Shortest-path rotation.
// ============================================================

const SVG_NS = 'http://www.w3.org/2000/svg';

// --- Brand palette (mirrors CSS :root) ---
const I = '#2B2D7C'; // indigo
const A = '#F5A623'; // amber
const V = '#E8471B'; // vermillion
const W = '#FFFFFF'; // white
const B = '#E8D5C0'; // beige

// --- Theme token keys ---
type TokenKey =
  | 'bg' | 'surface' | 'inset' | 'field'
  | 'onBg' | 'onSurface' | 'onInset' | 'onAccent'
  | 'heading' | 'accent' | 'danger'
  | 'border' | 'borderStrong' | 'stripe'
  | 'btnBg' | 'btnText';

interface ThemeDef {
  hour: number;
  name: string;
  tokens: Record<TokenKey, string>;
}

// --- 12 themes, indexed by hour (0 = 12 o'clock) ---
const THEMES: ThemeDef[] = [
  { hour: 0, name: 'noon', tokens:
    { bg: W, surface: W, inset: B, field: B, onBg: I, onSurface: I, onInset: I, onAccent: W, heading: V, accent: V, danger: V, border: B, borderStrong: V, stripe: V, btnBg: I, btnText: B } },
  { hour: 1, name: 'early-afternoon', tokens:
    { bg: W, surface: W, inset: B, field: B, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: B, borderStrong: I, stripe: A, btnBg: V, btnText: W } },
  { hour: 2, name: 'late-afternoon', tokens:
    { bg: B, surface: W, inset: B, field: W, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: W, borderStrong: I, stripe: A, btnBg: A, btnText: I } },
  { hour: 3, name: 'golden-hour', tokens:
    { bg: B, surface: W, inset: B, field: W, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: W, borderStrong: A, stripe: A, btnBg: V, btnText: W } },
  { hour: 4, name: 'sunset', tokens:
    { bg: B, surface: I, inset: W, field: W, onBg: I, onSurface: B, onInset: I, onAccent: W, heading: V, accent: V, danger: V, border: B, borderStrong: V, stripe: V, btnBg: A, btnText: I } },
  { hour: 5, name: 'dusk', tokens:
    { bg: I, surface: W, inset: B, field: W, onBg: B, onSurface: I, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: B, borderStrong: A, stripe: V, btnBg: V, btnText: W } },
  { hour: 6, name: 'twilight', tokens:
    { bg: I, surface: I, inset: W, field: W, onBg: B, onSurface: B, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: B, borderStrong: A, stripe: A, btnBg: V, btnText: W } },
  { hour: 7, name: 'evening', tokens:
    { bg: I, surface: I, inset: W, field: W, onBg: B, onSurface: B, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: B, borderStrong: B, stripe: A, btnBg: A, btnText: I } },
  { hour: 8, name: 'night', tokens:
    { bg: I, surface: I, inset: B, field: B, onBg: W, onSurface: W, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: B, borderStrong: A, stripe: V, btnBg: V, btnText: W } },
  { hour: 9, name: 'midnight', tokens:
    { bg: I, surface: I, inset: W, field: W, onBg: W, onSurface: W, onInset: I, onAccent: I, heading: W, accent: A, danger: V, border: B, borderStrong: A, stripe: V, btnBg: A, btnText: I } },
  { hour: 10, name: 'dawn', tokens:
    { bg: I, surface: B, inset: W, field: W, onBg: B, onSurface: I, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: W, borderStrong: A, stripe: A, btnBg: V, btnText: W } },
  { hour: 11, name: 'late-morning', tokens:
    { bg: W, surface: B, inset: W, field: W, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: B, borderStrong: I, stripe: I, btnBg: A, btnText: I } },
];

// --- Migration map: old 5-theme names → new names ---
const MIGRATION: Record<string, string> = {
  day: 'late-afternoon',
  dusk: 'twilight',
  'high-noon': 'noon',
  canyon: 'dusk',
  brand: 'sunset',
};

const STORAGE_KEY = 'ox:theme';
const DEFAULT_THEME = 'early-afternoon';

// --- Geometry helpers (exported for testing) ---

/** Angle in degrees for a given hour (0–11). 0 = 12 o'clock = 0deg. */
export function hourAngle(hour: number): number {
  return (hour % 12) * 30;
}

/** X,Y on a circle of given radius for an hour position. Center = (cx,cy). */
export function hourPosition(hour: number, cx: number, cy: number, r: number): { x: number; y: number } {
  const angle = ((hour % 12) * 30 - 90) * (Math.PI / 180);
  return {
    x: cx + r * Math.cos(angle),
    y: cy + r * Math.sin(angle),
  };
}

/**
 * Shortest-path rotation delta.
 * Returns a delta in [-180, 180) that gets from `from` to `to`.
 */
export function shortestRotation(from: number, to: number): number {
  return ((to - from + 540) % 360) - 180;
}

// --- Lookup ---

export function themeByName(name: string): ThemeDef | undefined {
  return THEMES.find((t) => t.name === name);
}

export function themeNames(): string[] {
  return THEMES.map((t) => t.name);
}

/**
 * Migrate an old theme name to a new one, or validate a new name.
 * Returns the resolved theme name or the default.
 */
export function migrateThemeName(raw: string | null): string {
  if (!raw) return DEFAULT_THEME;
  if (MIGRATION[raw]) return MIGRATION[raw];
  if (THEMES.some((t) => t.name === raw)) return raw;
  return DEFAULT_THEME;
}

// --- SVG Clock Builder ---

const CLOCK_SIZE = 80;
const VIEW = 100; // viewBox 0 0 100 100
const CX = 50;
const CY = 50;
const DOT_RADIUS_OUTER = 38; // hour dots sit on this circle
const DOT_R_INACTIVE = 3;
const DOT_R_ACTIVE = 4.5;
const HAND_LENGTH = 30;
const HIT_SIZE = 14; // invisible hit area per hour

function svgEl<K extends keyof SVGElementTagNameMap>(tag: K): SVGElementTagNameMap[K] {
  return document.createElementNS(SVG_NS, tag) as SVGElementTagNameMap[K];
}

function buildClock(
  activeTheme: string,
  onSelect: (name: string) => void,
): { svg: SVGSVGElement; setActive: (name: string) => void } {
  const svg = svgEl('svg');
  svg.setAttribute('viewBox', `0 0 ${VIEW} ${VIEW}`);
  svg.setAttribute('width', String(CLOCK_SIZE));
  svg.setAttribute('height', String(CLOCK_SIZE));
  svg.setAttribute('role', 'group');
  svg.setAttribute('aria-label', 'Theme clock');
  svg.classList.add('clock');

  // Face
  const face = svgEl('circle');
  face.setAttribute('cx', String(CX));
  face.setAttribute('cy', String(CY));
  face.setAttribute('r', '45');
  face.classList.add('clock-face');
  svg.appendChild(face);

  // Hand
  const handGroup = svgEl('g');
  handGroup.classList.add('clock-hand-group');
  const hand = svgEl('line');
  hand.setAttribute('x1', String(CX));
  hand.setAttribute('y1', String(CY));
  hand.setAttribute('x2', String(CX));
  hand.setAttribute('y2', String(CY - HAND_LENGTH));
  hand.classList.add('clock-hand');
  handGroup.appendChild(hand);
  svg.appendChild(handGroup);

  // Center dot
  const center = svgEl('circle');
  center.setAttribute('cx', String(CX));
  center.setAttribute('cy', String(CY));
  center.setAttribute('r', '3');
  center.classList.add('clock-center');
  svg.appendChild(center);

  // Hour groups (dots + hit areas)
  const hourGroups: SVGGElement[] = [];
  for (const theme of THEMES) {
    const pos = hourPosition(theme.hour, CX, CY, DOT_RADIUS_OUTER);
    const g = svgEl('g');
    g.setAttribute('role', 'button');
    g.setAttribute('tabindex', '0');
    g.setAttribute('aria-label', theme.name);
    g.dataset.themeName = theme.name;
    g.classList.add('clock-hour');

    // Invisible hit area
    const hitRect = svgEl('rect');
    hitRect.setAttribute('x', String(pos.x - HIT_SIZE / 2));
    hitRect.setAttribute('y', String(pos.y - HIT_SIZE / 2));
    hitRect.setAttribute('width', String(HIT_SIZE));
    hitRect.setAttribute('height', String(HIT_SIZE));
    hitRect.setAttribute('fill', 'transparent');
    hitRect.classList.add('clock-hit');
    g.appendChild(hitRect);

    // Dot
    const dot = svgEl('circle');
    dot.setAttribute('cx', String(pos.x));
    dot.setAttribute('cy', String(pos.y));
    const isActive = theme.name === activeTheme;
    dot.setAttribute('r', String(isActive ? DOT_R_ACTIVE : DOT_R_INACTIVE));
    dot.classList.add('clock-dot');
    if (isActive) dot.classList.add('clock-dot-active');
    g.appendChild(dot);

    g.addEventListener('click', () => onSelect(theme.name));
    g.addEventListener('keydown', (e) => {
      if ((e as KeyboardEvent).key === 'Enter' || (e as KeyboardEvent).key === ' ') {
        e.preventDefault();
        onSelect(theme.name);
      }
    });

    svg.appendChild(g);
    hourGroups.push(g);
  }

  // Set initial hand rotation
  const initialHour = THEMES.find((t) => t.name === activeTheme)?.hour ?? 1;
  let currentAngle = hourAngle(initialHour);
  handGroup.style.transform = `rotate(${currentAngle}deg)`;
  handGroup.style.transformOrigin = `${CX}px ${CY}px`;

  function setActive(name: string): void {
    const theme = THEMES.find((t) => t.name === name);
    if (!theme) return;

    const targetAngle = hourAngle(theme.hour);
    const delta = shortestRotation(currentAngle, targetAngle);
    currentAngle += delta;
    handGroup.style.transform = `rotate(${currentAngle}deg)`;

    for (const g of hourGroups) {
      const dot = g.querySelector('.clock-dot') as SVGCircleElement | null;
      if (!dot) continue;
      const isThis = g.dataset.themeName === name;
      dot.setAttribute('r', String(isThis ? DOT_R_ACTIVE : DOT_R_INACTIVE));
      if (isThis) {
        dot.classList.add('clock-dot-active');
      } else {
        dot.classList.remove('clock-dot-active');
      }
    }
  }

  return { svg, setActive };
}

// --- Apply theme to DOM ---

function applyTheme(name: string): void {
  document.body.dataset.theme = name;
  localStorage.setItem(STORAGE_KEY, name);
}

// --- Public init ---

export function initThemePicker(): void {
  // Migrate saved theme
  const raw = localStorage.getItem(STORAGE_KEY);
  const initial = migrateThemeName(raw);

  // Persist migrated name (clears old value)
  applyTheme(initial);

  const picker = document.getElementById('theme-picker');
  if (!picker) return;

  const { svg, setActive } = buildClock(initial, (name) => {
    applyTheme(name);
    setActive(name);
  });

  picker.appendChild(svg);
}
