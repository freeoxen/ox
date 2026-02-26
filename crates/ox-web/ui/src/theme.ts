// ============================================================
// ox — Analog Clock Theme Picker
// 12 themes, one per hour. SVG clock face. Shortest-path rotation.
// ============================================================

const SVG_NS = 'http://www.w3.org/2000/svg';

// --- Brand palette (mirrors CSS :root) ---
const I = '#2B2D7C'; // indigo
const S = '#6B6D8F'; // slate
const V = '#E8471B'; // vermillion
const C = '#B8926E'; // clay
const A = '#F5A623'; // amber
const B = '#E8D5C0'; // beige
const W = '#FFFFFF'; // white

// --- Theme token keys ---
type TokenKey =
  | 'bg' | 'surface' | 'inset' | 'field'
  | 'onBg' | 'onSurface' | 'onInset' | 'onAccent'
  | 'heading' | 'accent' | 'danger'
  | 'border' | 'borderStrong' | 'stripe'
  | 'btnBg' | 'btnText'
  | 'muted' | 'faint';

interface ThemeDef {
  hour: number;
  name: string;
  desc: string;
  tokens: Record<TokenKey, string>;
}

// --- 12 themes, indexed by hour (0 = 12 o'clock) ---
const THEMES: ThemeDef[] = [
  { hour: 0, name: 'noon', desc: 'Stark white, vermillion burns', tokens:
    { bg: W, surface: W, inset: B, field: B, onBg: I, onSurface: I, onInset: I, onAccent: W, heading: V, accent: V, danger: V, border: S, borderStrong: I, stripe: V, btnBg: I, btnText: W, muted: S, faint: C } },
  { hour: 1, name: 'early-afternoon', desc: 'White canvas, amber warms in', tokens:
    { bg: W, surface: W, inset: B, field: B, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: C, borderStrong: I, stripe: A, btnBg: V, btnText: W, muted: S, faint: C } },
  { hour: 2, name: 'late-afternoon', desc: 'Beige earth appears', tokens:
    { bg: B, surface: W, inset: B, field: W, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: S, borderStrong: I, stripe: A, btnBg: A, btnText: I, muted: S, faint: C } },
  { hour: 3, name: 'golden-hour', desc: 'Everything amber, warmest light', tokens:
    { bg: B, surface: W, inset: B, field: W, onBg: I, onSurface: I, onInset: I, onAccent: I, heading: I, accent: A, danger: V, border: S, borderStrong: I, stripe: A, btnBg: V, btnText: W, muted: S, faint: C } },
  { hour: 4, name: 'sunset', desc: 'Beige sky, indigo shadows rise', tokens:
    { bg: B, surface: I, inset: W, field: W, onBg: I, onSurface: B, onInset: I, onAccent: I, heading: V, accent: A, danger: V, border: S, borderStrong: V, stripe: A, btnBg: V, btnText: W, muted: C, faint: S } },
  { hour: 5, name: 'dusk', desc: 'Indigo canvas, white surfaces float', tokens:
    { bg: I, surface: W, inset: B, field: W, onBg: B, onSurface: I, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: S, borderStrong: A, stripe: V, btnBg: V, btnText: W, muted: S, faint: C } },
  { hour: 6, name: 'twilight', desc: 'Full indigo, amber the only warmth', tokens:
    { bg: I, surface: I, inset: W, field: W, onBg: B, onSurface: B, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: S, borderStrong: A, stripe: A, btnBg: V, btnText: W, muted: C, faint: S } },
  { hour: 7, name: 'evening', desc: 'Softer borders, settling in', tokens:
    { bg: I, surface: I, inset: B, field: B, onBg: B, onSurface: B, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: S, borderStrong: B, stripe: A, btnBg: A, btnText: I, muted: C, faint: S } },
  { hour: 8, name: 'night', desc: 'White text, beige warms the dark', tokens:
    { bg: I, surface: I, inset: B, field: B, onBg: W, onSurface: W, onInset: I, onAccent: I, heading: A, accent: A, danger: V, border: S, borderStrong: A, stripe: V, btnBg: V, btnText: W, muted: C, faint: S } },
  { hour: 9, name: 'midnight', desc: 'The coldest hour', tokens:
    { bg: I, surface: I, inset: W, field: W, onBg: W, onSurface: W, onInset: I, onAccent: I, heading: W, accent: A, danger: V, border: S, borderStrong: A, stripe: V, btnBg: A, btnText: I, muted: C, faint: S } },
  { hour: 10, name: 'dawn', desc: 'Beige surfaces emerge, first light', tokens:
    { bg: I, surface: B, inset: W, field: W, onBg: B, onSurface: I, onInset: I, onAccent: W, heading: A, accent: V, danger: V, border: S, borderStrong: A, stripe: A, btnBg: A, btnText: I, muted: S, faint: C } },
  { hour: 11, name: 'late-morning', desc: 'Brightening toward noon', tokens:
    { bg: W, surface: B, inset: W, field: W, onBg: I, onSurface: I, onInset: I, onAccent: W, heading: I, accent: V, danger: V, border: S, borderStrong: I, stripe: I, btnBg: A, btnText: I, muted: S, faint: C } },
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

/**
 * Convert a clock-face angle (0 = 12 o'clock, clockwise degrees) to
 * the nearest hour (0–11). Rounds to the closest 30-degree sector.
 */
export function angleToHour(degrees: number): number {
  const norm = ((degrees % 360) + 360) % 360;
  return Math.round(norm / 30) % 12;
}

// --- Wall-clock mapping ---

/**
 * Maps each of the 24 system hours to one of the 12 theme hours.
 * Compresses a full day into the clock's narrative arc:
 * noon → afternoon → sunset → night → midnight → dawn → morning
 */
const WALL_MAP: readonly number[] = [
//  0h  1h  2h  3h  4h  5h  6h  7h  8h  9h 10h 11h
     9,  9,  9,  9, 10, 10, 10, 11, 11, 11, 11, 11,
// 12h 13h 14h 15h 16h 17h 18h 19h 20h 21h 22h 23h
     0,  1,  1,  2,  3,  4,  5,  6,  7,  8,  8,  9,
];

/** Map a 24-hour system hour (0–23) to a theme hour (0–11). */
export function wallClockToThemeHour(systemHour: number): number {
  return WALL_MAP[((systemHour % 24) + 24) % 24];
}

const MODE_KEY = 'ox:clock-mode';

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

const CLOCK_SIZE = 54;
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
  svg.setAttribute('aria-label', 'Theme clock — drag or click an hour to switch themes');
  svg.classList.add('clock');

  const svgTitle = svgEl('title');
  svgTitle.textContent = 'Theme clock — drag or click an hour to switch themes';
  svg.appendChild(svgTitle);

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
    g.setAttribute('aria-label', `${theme.name} — ${theme.desc}`);
    g.dataset.themeName = theme.name;
    g.dataset.themeDesc = theme.desc;
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
  let activeHour = initialHour;
  handGroup.style.transform = `rotate(${currentAngle}deg)`;
  handGroup.style.transformOrigin = `${CX}px ${CY}px`;

  function setActive(name: string): void {
    const theme = THEMES.find((t) => t.name === name);
    if (!theme) return;

    activeHour = theme.hour;
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

  // --- Drag interaction ---

  let dragging = false;

  /** Convert a pointer event to a clock-face angle (0 = 12 o'clock, CW). */
  function pointerToClockAngle(e: PointerEvent): number {
    const rect = svg.getBoundingClientRect();
    const svgX = (e.clientX - rect.left) / rect.width * VIEW;
    const svgY = (e.clientY - rect.top) / rect.height * VIEW;
    const dx = svgX - CX;
    const dy = -(svgY - CY); // flip Y so up is positive
    // atan2 with (dx, dy) gives 0 = up, positive = clockwise
    const rad = Math.atan2(dx, dy);
    return ((rad * 180 / Math.PI) + 360) % 360;
  }

  function handleDragMove(e: PointerEvent): void {
    const angle = pointerToClockAngle(e);
    const hour = angleToHour(angle);
    if (hour !== activeHour) {
      const theme = THEMES[hour];
      if (theme) onSelect(theme.name);
    }
  }

  svg.addEventListener('pointerdown', (e: Event) => {
    const pe = e as PointerEvent;
    dragging = true;
    svg.classList.add('clock-dragging');
    handGroup.classList.add('clock-hand-no-transition');
    svg.setPointerCapture(pe.pointerId);
    handleDragMove(pe);
  });

  svg.addEventListener('pointermove', (e: Event) => {
    if (!dragging) return;
    handleDragMove(e as PointerEvent);
  });

  svg.addEventListener('pointerup', (e: Event) => {
    if (!dragging) return;
    dragging = false;
    svg.classList.remove('clock-dragging');
    handGroup.classList.remove('clock-hand-no-transition');
    svg.releasePointerCapture((e as PointerEvent).pointerId);
  });

  svg.addEventListener('pointercancel', (e: Event) => {
    if (!dragging) return;
    dragging = false;
    svg.classList.remove('clock-dragging');
    handGroup.classList.remove('clock-hand-no-transition');
    svg.releasePointerCapture((e as PointerEvent).pointerId);
  });

  return { svg, setActive };
}

// --- Mode toggle icons ---

function sunIcon(): SVGSVGElement {
  const s = svgEl('svg');
  s.setAttribute('viewBox', '0 0 16 16');
  s.setAttribute('width', '14');
  s.setAttribute('height', '14');
  s.classList.add('clock-mode-icon');

  const c = svgEl('circle');
  c.setAttribute('cx', '8');
  c.setAttribute('cy', '8');
  c.setAttribute('r', '2.5');
  c.setAttribute('fill', 'currentColor');
  s.appendChild(c);

  const rays: [number, number, number, number][] = [
    [8, 1.5, 8, 4],   [8, 12, 8, 14.5],
    [1.5, 8, 4, 8],   [12, 8, 14.5, 8],
    [3.6, 3.6, 5.1, 5.1], [10.9, 10.9, 12.4, 12.4],
    [3.6, 12.4, 5.1, 10.9], [10.9, 5.1, 12.4, 3.6],
  ];
  for (const [x1, y1, x2, y2] of rays) {
    const l = svgEl('line');
    l.setAttribute('x1', String(x1));
    l.setAttribute('y1', String(y1));
    l.setAttribute('x2', String(x2));
    l.setAttribute('y2', String(y2));
    l.setAttribute('stroke', 'currentColor');
    l.setAttribute('stroke-width', '1.5');
    l.setAttribute('stroke-linecap', 'round');
    s.appendChild(l);
  }
  return s;
}

function wandIcon(): SVGSVGElement {
  const s = svgEl('svg');
  s.setAttribute('viewBox', '0 0 16 16');
  s.setAttribute('width', '14');
  s.setAttribute('height', '14');
  s.classList.add('clock-mode-icon');

  // Wand shaft
  const shaft = svgEl('line');
  shaft.setAttribute('x1', '2');
  shaft.setAttribute('y1', '14');
  shaft.setAttribute('x2', '9.5');
  shaft.setAttribute('y2', '6.5');
  shaft.setAttribute('stroke', 'currentColor');
  shaft.setAttribute('stroke-width', '2');
  shaft.setAttribute('stroke-linecap', 'round');
  s.appendChild(shaft);

  // Sparkle cross at tip
  const sparkles: [number, number, number, number][] = [
    [12, 1.5, 12, 6.5],
    [9.5, 4, 14.5, 4],
  ];
  for (const [x1, y1, x2, y2] of sparkles) {
    const l = svgEl('line');
    l.setAttribute('x1', String(x1));
    l.setAttribute('y1', String(y1));
    l.setAttribute('x2', String(x2));
    l.setAttribute('y2', String(y2));
    l.setAttribute('stroke', 'currentColor');
    l.setAttribute('stroke-width', '1.2');
    l.setAttribute('stroke-linecap', 'round');
    s.appendChild(l);
  }

  // Small sparkle dots
  for (const [cx, cy] of [[10, 2], [14.2, 6]] as [number, number][]) {
    const d = svgEl('circle');
    d.setAttribute('cx', String(cx));
    d.setAttribute('cy', String(cy));
    d.setAttribute('r', '0.8');
    d.setAttribute('fill', 'currentColor');
    s.appendChild(d);
  }
  return s;
}

// --- Apply theme to DOM ---

function applyTheme(name: string): void {
  document.documentElement.dataset.theme = name;
  document.body.dataset.theme = name;
  localStorage.setItem(STORAGE_KEY, name);
}

// --- Public init ---

export function initThemePicker(): void {
  const raw = localStorage.getItem(STORAGE_KEY);
  const migratedName = migrateThemeName(raw);
  const savedMode = localStorage.getItem(MODE_KEY);
  let mode: 'wall' | 'manual' = savedMode === 'wall' ? 'wall' : 'manual';

  const wallName = (): string =>
    THEMES[wallClockToThemeHour(new Date().getHours())].name;

  const initialTheme = mode === 'wall' ? wallName() : migratedName;
  applyTheme(initialTheme);

  const picker = document.getElementById('theme-picker');
  if (!picker) return;
  picker.title = 'Theme picker';

  let wallInterval: ReturnType<typeof setInterval> | null = null;

  const { svg, setActive } = buildClock(initialTheme, (name) => {
    // Any manual interaction exits wall-time mode
    if (mode === 'wall') {
      switchMode('manual');
    }
    applyTheme(name);
    setActive(name);
  });

  // --- Mode toggle button ---

  const toggle = document.createElement('button');
  toggle.className = 'clock-mode-toggle';

  function updateToggle(): void {
    toggle.innerHTML = '';
    if (mode === 'wall') {
      toggle.appendChild(sunIcon());
      toggle.title = 'Following time of day';
      toggle.setAttribute('aria-label', 'Switch to manual theme');
      svg.classList.add('clock-live');
    } else {
      toggle.appendChild(wandIcon());
      toggle.title = 'Manual theme';
      toggle.setAttribute('aria-label', 'Follow time of day');
      svg.classList.remove('clock-live');
    }
  }

  function startWall(): void {
    const tick = (): void => {
      const name = wallName();
      applyTheme(name);
      setActive(name);
    };
    tick();
    wallInterval = setInterval(tick, 60_000);
  }

  function stopWall(): void {
    if (wallInterval !== null) {
      clearInterval(wallInterval);
      wallInterval = null;
    }
  }

  function switchMode(newMode: 'wall' | 'manual'): void {
    mode = newMode;
    localStorage.setItem(MODE_KEY, mode);
    if (mode === 'wall') {
      startWall();
    } else {
      stopWall();
    }
    updateToggle();
  }

  toggle.addEventListener('click', () => {
    switchMode(mode === 'wall' ? 'manual' : 'wall');
  });

  updateToggle();
  picker.textContent = '';
  picker.appendChild(svg);
  picker.appendChild(toggle);

  if (mode === 'wall') {
    startWall();
  }
}
