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

// --- Motion demos (easing tracks) ---

function initMotionDemos(): void {
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const track = entry.target as HTMLElement;
        if (entry.isIntersecting) {
          const easing = track.dataset.easing ?? "ease";
          const duration = track.dataset.duration ?? "1.2s";
          track.style.setProperty("--easing-fn", easing);
          track.style.setProperty("--easing-duration", duration);
          track.classList.add("easing-active");
        } else {
          track.classList.remove("easing-active");
        }
      }
    },
    { threshold: 0.3 },
  );

  for (const el of document.querySelectorAll(".easing-track")) {
    observer.observe(el);
  }
}

// --- Init ---

document.addEventListener("DOMContentLoaded", () => {
  initThemePicker();
  initScrollAnimations();
  initThemeChips();
  initMotionDemos();
});
