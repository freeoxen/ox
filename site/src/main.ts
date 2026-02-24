import { initThemePicker, themeByName } from "../../crates/ox-web/ui/src/theme";

// --- Scroll animations ---

function initScrollAnimations(): void {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add("visible");
          observer.unobserve(entry.target);
        }
      }
    },
    { threshold: 0.15 },
  );

  for (const el of document.querySelectorAll(".animate-in")) {
    observer.observe(el);
  }
}

// --- Theme chips ---

function initThemeChips(): void {
  for (const chip of document.querySelectorAll<HTMLButtonElement>(
    ".theme-chip",
  )) {
    const name = chip.dataset.theme;
    if (!name) continue;

    // Add descriptor as title
    const def = themeByName(name);
    if (def) chip.title = def.desc;

    chip.addEventListener("click", () => {
      // Find the matching clock hour element and dispatch a click
      const hour = document.querySelector<SVGGElement>(
        `.clock-hour[data-theme-name="${name}"]`,
      );
      if (hour) {
        hour.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      }
    });
  }
}

// --- Init ---

document.addEventListener("DOMContentLoaded", () => {
  initThemePicker();
  initScrollAnimations();
  initThemeChips();
});
