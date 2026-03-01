// Svelte store layer on top of pure theme data.
// Re-exports everything from theme-data, plus adds reactive store bindings.

import { writable } from "svelte/store";
import { applyThemeDOM } from "../theme-data";

// Re-export all pure functions and data
export {
  THEMES,
  hourAngle,
  hourPosition,
  shortestRotation,
  angleToHour,
  wallClockToThemeHour,
  themeByName,
  themeNames,
  migrateThemeName,
  initThemePicker,
  loadSavedTheme,
} from "../theme-data";
export type { ThemeDef } from "../theme-data";

// --- Svelte store ---

export const currentTheme = writable<string>("early-afternoon");

export function applyTheme(name: string): void {
  if (typeof document !== "undefined") {
    applyThemeDOM(name);
  }
  currentTheme.set(name);
}
