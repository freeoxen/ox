import { describe, expect, test, beforeEach } from "bun:test";
import {
  hourAngle,
  hourPosition,
  shortestRotation,
  angleToHour,
  wallClockToThemeHour,
  migrateThemeName,
  themeByName,
  themeNames,
  initThemePicker,
} from "./theme";

// --- Geometry ---

describe("hourAngle", () => {
  test("12 oclock (hour 0) is 0 degrees", () => {
    expect(hourAngle(0)).toBe(0);
  });

  test("3 oclock (hour 3) is 90 degrees", () => {
    expect(hourAngle(3)).toBe(90);
  });

  test("6 oclock (hour 6) is 180 degrees", () => {
    expect(hourAngle(6)).toBe(180);
  });

  test("9 oclock (hour 9) is 270 degrees", () => {
    expect(hourAngle(9)).toBe(270);
  });

  test("wraps past 12", () => {
    expect(hourAngle(12)).toBe(0);
    expect(hourAngle(15)).toBe(90);
  });
});

describe("hourPosition", () => {
  const cx = 50,
    cy = 50,
    r = 38;

  test("hour 0 (12 oclock) is straight up", () => {
    const pos = hourPosition(0, cx, cy, r);
    expect(pos.x).toBeCloseTo(50, 5);
    expect(pos.y).toBeCloseTo(12, 5);
  });

  test("hour 3 is to the right", () => {
    const pos = hourPosition(3, cx, cy, r);
    expect(pos.x).toBeCloseTo(88, 5);
    expect(pos.y).toBeCloseTo(50, 5);
  });

  test("hour 6 is straight down", () => {
    const pos = hourPosition(6, cx, cy, r);
    expect(pos.x).toBeCloseTo(50, 5);
    expect(pos.y).toBeCloseTo(88, 5);
  });

  test("hour 9 is to the left", () => {
    const pos = hourPosition(9, cx, cy, r);
    expect(pos.x).toBeCloseTo(12, 5);
    expect(pos.y).toBeCloseTo(50, 5);
  });
});

// --- Shortest-path rotation ---

describe("shortestRotation", () => {
  test("same angle is 0 delta", () => {
    expect(shortestRotation(90, 90)).toBe(0);
  });

  test("clockwise short path", () => {
    expect(shortestRotation(0, 90)).toBe(90);
  });

  test("counter-clockwise short path", () => {
    expect(shortestRotation(90, 0)).toBe(-90);
  });

  test("wraps clockwise across 360/0 boundary", () => {
    // from 330 to 30: shortest is +60 (clockwise)
    expect(shortestRotation(330, 30)).toBe(60);
  });

  test("wraps counter-clockwise across 360/0 boundary", () => {
    // from 30 to 330: shortest is -60 (counter-clockwise)
    expect(shortestRotation(30, 330)).toBe(-60);
  });

  test("opposite sides prefer positive 180", () => {
    // 180 away: convention is ((180+540)%360)-180 = 180
    const d = shortestRotation(0, 180);
    expect(Math.abs(d)).toBe(180);
  });
});

// --- angleToHour ---

describe("angleToHour", () => {
  test("0 degrees is hour 0 (12 oclock)", () => {
    expect(angleToHour(0)).toBe(0);
  });

  test("90 degrees is hour 3", () => {
    expect(angleToHour(90)).toBe(3);
  });

  test("180 degrees is hour 6", () => {
    expect(angleToHour(180)).toBe(6);
  });

  test("270 degrees is hour 9", () => {
    expect(angleToHour(270)).toBe(9);
  });

  test("snaps to nearest hour", () => {
    // 14 degrees is closer to 0 (0deg) than 1 (30deg)
    expect(angleToHour(14)).toBe(0);
    // 16 degrees is closer to 1 (30deg) than 0 (0deg)
    expect(angleToHour(16)).toBe(1);
    // exactly on boundary rounds up
    expect(angleToHour(15)).toBe(1);
  });

  test("handles negative angles", () => {
    expect(angleToHour(-30)).toBe(11);
    expect(angleToHour(-90)).toBe(9);
  });

  test("handles angles > 360", () => {
    expect(angleToHour(390)).toBe(1);
    expect(angleToHour(720)).toBe(0);
  });

  test("355 degrees snaps to hour 0", () => {
    expect(angleToHour(355)).toBe(0);
  });

  test("roundtrip: hourAngle then angleToHour returns same hour", () => {
    for (let h = 0; h < 12; h++) {
      expect(angleToHour(hourAngle(h))).toBe(h);
    }
  });
});

// --- Wall clock mapping ---

describe("wallClockToThemeHour", () => {
  test("noon (12h) maps to theme hour 0", () => {
    expect(wallClockToThemeHour(12)).toBe(0);
  });

  test("1pm (13h) maps to early-afternoon (1)", () => {
    expect(wallClockToThemeHour(13)).toBe(1);
  });

  test("5pm (17h) maps to sunset (4)", () => {
    expect(wallClockToThemeHour(17)).toBe(4);
  });

  test("midnight (0h) maps to theme midnight (9)", () => {
    expect(wallClockToThemeHour(0)).toBe(9);
  });

  test("5am maps to dawn (10)", () => {
    expect(wallClockToThemeHour(5)).toBe(10);
  });

  test("9am maps to late-morning (11)", () => {
    expect(wallClockToThemeHour(9)).toBe(11);
  });

  test("handles negative input", () => {
    expect(wallClockToThemeHour(-1)).toBe(wallClockToThemeHour(23));
  });

  test("all 24 hours produce valid theme hours (0-11)", () => {
    for (let h = 0; h < 24; h++) {
      const th = wallClockToThemeHour(h);
      expect(th).toBeGreaterThanOrEqual(0);
      expect(th).toBeLessThan(12);
    }
  });
});

// --- Migration ---

describe("migrateThemeName", () => {
  test("null returns default", () => {
    expect(migrateThemeName(null)).toBe("early-afternoon");
  });

  test("empty string returns default", () => {
    expect(migrateThemeName("")).toBe("early-afternoon");
  });

  test("unknown name returns default", () => {
    expect(migrateThemeName("nonexistent")).toBe("early-afternoon");
  });

  test('migrates old "day" to "late-afternoon"', () => {
    expect(migrateThemeName("day")).toBe("late-afternoon");
  });

  test('migrates old "dusk" to "twilight"', () => {
    expect(migrateThemeName("dusk")).toBe("twilight");
  });

  test('migrates old "high-noon" to "noon"', () => {
    expect(migrateThemeName("high-noon")).toBe("noon");
  });

  test('migrates old "canyon" to "dusk"', () => {
    expect(migrateThemeName("canyon")).toBe("dusk");
  });

  test('migrates old "brand" to "sunset"', () => {
    expect(migrateThemeName("brand")).toBe("sunset");
  });

  test("keeps valid new name as-is", () => {
    expect(migrateThemeName("midnight")).toBe("midnight");
    expect(migrateThemeName("dawn")).toBe("dawn");
    expect(migrateThemeName("noon")).toBe("noon");
  });
});

// --- Theme data ---

describe("themeNames", () => {
  test("returns 12 theme names", () => {
    expect(themeNames()).toHaveLength(12);
  });

  test("includes all expected names", () => {
    const names = themeNames();
    for (const n of [
      "noon",
      "early-afternoon",
      "late-afternoon",
      "golden-hour",
      "sunset",
      "dusk",
      "twilight",
      "evening",
      "night",
      "midnight",
      "dawn",
      "late-morning",
    ]) {
      expect(names).toContain(n);
    }
  });
});

describe("themeByName", () => {
  test("finds noon at hour 0", () => {
    const t = themeByName("noon");
    expect(t).toBeDefined();
    expect(t!.hour).toBe(0);
  });

  test("returns undefined for unknown", () => {
    expect(themeByName("xyz")).toBeUndefined();
  });

  test("every theme has all 18 token keys", () => {
    const keys = [
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
    for (const name of themeNames()) {
      const t = themeByName(name)!;
      for (const k of keys) {
        expect(t.tokens).toHaveProperty(k);
      }
    }
  });
});

// --- DOM: initThemePicker ---

describe("initThemePicker", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    localStorage.clear();
    const picker = document.createElement("div");
    picker.id = "theme-picker";
    document.body.appendChild(picker);
  });

  test("creates an SVG inside #theme-picker", () => {
    initThemePicker();
    const svg = document.querySelector("#theme-picker svg");
    expect(svg).not.toBeNull();
  });

  test("SVG has 12 hour groups", () => {
    initThemePicker();
    const groups = document.querySelectorAll(".clock-hour");
    expect(groups.length).toBe(12);
  });

  test("each hour group has role=button and aria-label", () => {
    initThemePicker();
    const groups = document.querySelectorAll(".clock-hour");
    for (const g of groups) {
      expect(g.getAttribute("role")).toBe("button");
      expect(g.getAttribute("aria-label")).toBeTruthy();
    }
  });

  test("sets data-theme on html and body", () => {
    initThemePicker();
    expect(document.documentElement.dataset.theme).toBeTruthy();
    expect(document.body.dataset.theme).toBeTruthy();
  });

  test("defaults to early-afternoon when no saved theme", () => {
    initThemePicker();
    expect(document.documentElement.dataset.theme).toBe("early-afternoon");
  });

  test("migrates old saved theme", () => {
    localStorage.setItem("ox:theme", "high-noon");
    initThemePicker();
    expect(document.documentElement.dataset.theme).toBe("noon");
    expect(localStorage.getItem("ox:theme")).toBe("noon");
  });

  test("preserves valid new theme name", () => {
    localStorage.setItem("ox:theme", "midnight");
    initThemePicker();
    expect(document.documentElement.dataset.theme).toBe("midnight");
  });

  test("clicking an hour group switches theme", () => {
    initThemePicker();
    const twilight = document.querySelector(
      '[data-theme-name="twilight"]',
    ) as Element;
    expect(twilight).not.toBeNull();
    twilight.dispatchEvent(new Event("click", { bubbles: true }));
    expect(document.documentElement.dataset.theme).toBe("twilight");
    expect(localStorage.getItem("ox:theme")).toBe("twilight");
  });

  test("has a clock face, hand, and center dot", () => {
    initThemePicker();
    expect(document.querySelector(".clock-face")).not.toBeNull();
    expect(document.querySelector(".clock-hand")).not.toBeNull();
    expect(document.querySelector(".clock-center")).not.toBeNull();
  });

  test("adds clock-dragging class on pointerdown", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    expect(svg).not.toBeNull();
    // happy-dom may not fully support pointer events, but we can dispatch
    svg.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(svg.classList.contains("clock-dragging")).toBe(true);
  });

  test("removes clock-dragging class on pointerup", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    svg.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(svg.classList.contains("clock-dragging")).toBe(true);
    svg.dispatchEvent(new Event("pointerup", { bubbles: true }));
    expect(svg.classList.contains("clock-dragging")).toBe(false);
  });

  test("hand group gets no-transition class during drag", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    const handGroup = document.querySelector(".clock-hand-group") as Element;
    svg.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    expect(handGroup.classList.contains("clock-hand-no-transition")).toBe(true);
    svg.dispatchEvent(new Event("pointerup", { bubbles: true }));
    expect(handGroup.classList.contains("clock-hand-no-transition")).toBe(
      false,
    );
  });

  test("renders a mode toggle button", () => {
    initThemePicker();
    const toggle = document.querySelector(".clock-mode-toggle");
    expect(toggle).not.toBeNull();
    expect(toggle!.tagName).toBe("BUTTON");
  });

  test("toggle button contains an icon SVG", () => {
    initThemePicker();
    const icon = document.querySelector(".clock-mode-toggle .clock-mode-icon");
    expect(icon).not.toBeNull();
  });

  test("defaults to manual mode (wand icon, no clock-live)", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    expect(svg.classList.contains("clock-live")).toBe(false);
  });

  test("clicking toggle switches to wall mode", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    const toggle = document.querySelector(".clock-mode-toggle") as Element;
    toggle.dispatchEvent(new Event("click", { bubbles: true }));
    expect(svg.classList.contains("clock-live")).toBe(true);
    expect(localStorage.getItem("ox:clock-mode")).toBe("wall");
  });

  test("clicking toggle twice returns to manual mode", () => {
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    const toggle = document.querySelector(".clock-mode-toggle") as Element;
    toggle.dispatchEvent(new Event("click", { bubbles: true }));
    toggle.dispatchEvent(new Event("click", { bubbles: true }));
    expect(svg.classList.contains("clock-live")).toBe(false);
    expect(localStorage.getItem("ox:clock-mode")).toBe("manual");
  });

  test("restores wall mode from localStorage", () => {
    localStorage.setItem("ox:clock-mode", "wall");
    initThemePicker();
    const svg = document.querySelector(".clock") as Element;
    expect(svg.classList.contains("clock-live")).toBe(true);
  });
});
