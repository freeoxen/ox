<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import {
    THEMES,
    hourAngle,
    hourPosition,
    shortestRotation,
    angleToHour,
    wallClockToThemeHour,
    currentTheme,
    applyTheme,
    loadSavedTheme,
  } from "$lib/stores/theme";

  const CX = 50;
  const CY = 50;
  const DOT_RADIUS_OUTER = 38;
  const DOT_R_INACTIVE = 3;
  const DOT_R_ACTIVE = 4.5;
  const HIT_SIZE = 14;
  const MODE_KEY = "ox:clock-mode";

  const sunRays: [number, number, number, number][] = [
    [8, 1.5, 8, 4],
    [8, 12, 8, 14.5],
    [1.5, 8, 4, 8],
    [12, 8, 14.5, 8],
    [3.6, 3.6, 5.1, 5.1],
    [10.9, 10.9, 12.4, 12.4],
    [3.6, 12.4, 5.1, 10.9],
    [10.9, 5.1, 12.4, 3.6],
  ];

  let svgEl: SVGSVGElement | undefined = $state();
  let dragging = $state(false);
  let mode = $state<"wall" | "manual">("manual");
  let currentAngle = $state(0);
  let activeHour = $state(0);
  let wallInterval: ReturnType<typeof setInterval> | null = null;

  function wallName(): string {
    return THEMES[wallClockToThemeHour(new Date().getHours())].name;
  }

  function selectTheme(name: string) {
    if (mode === "wall") switchMode("manual");
    applyTheme(name);
    setActive(name);
  }

  function setActive(name: string) {
    const theme = THEMES.find((t) => t.name === name);
    if (!theme) return;
    activeHour = theme.hour;
    const targetAngle = hourAngle(theme.hour);
    const delta = shortestRotation(currentAngle, targetAngle);
    currentAngle += delta;
  }

  function handleKeydown(e: KeyboardEvent, name: string) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      selectTheme(name);
    }
  }

  function pointerToClockAngle(e: PointerEvent): number {
    if (!svgEl) return 0;
    const rect = svgEl.getBoundingClientRect();
    const svgX = ((e.clientX - rect.left) / rect.width) * 100;
    const svgY = ((e.clientY - rect.top) / rect.height) * 100;
    const dx = svgX - CX;
    const dy = -(svgY - CY);
    const rad = Math.atan2(dx, dy);
    return ((rad * 180) / Math.PI + 360) % 360;
  }

  function handleDragMove(e: PointerEvent) {
    const angle = pointerToClockAngle(e);
    const hour = angleToHour(angle);
    if (hour !== activeHour) {
      const theme = THEMES[hour];
      if (theme) selectTheme(theme.name);
    }
  }

  function handlePointerDown(e: PointerEvent) {
    dragging = true;
    svgEl?.setPointerCapture(e.pointerId);
    handleDragMove(e);
  }

  function handlePointerMove(e: PointerEvent) {
    if (!dragging) return;
    handleDragMove(e);
  }

  function handlePointerUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    svgEl?.releasePointerCapture(e.pointerId);
  }

  function startWall() {
    const tick = () => {
      const name = wallName();
      applyTheme(name);
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
    } else {
      stopWall();
    }
  }

  function toggleMode() {
    switchMode(mode === "wall" ? "manual" : "wall");
  }

  onMount(() => {
    const savedMode = localStorage.getItem(MODE_KEY);
    mode = savedMode === "wall" ? "wall" : "manual";

    const initialTheme = mode === "wall" ? wallName() : loadSavedTheme();
    applyTheme(initialTheme);
    setActive(initialTheme);

    if (mode === "wall") startWall();
  });

  onDestroy(() => {
    stopWall();
  });
</script>

<div id="theme-picker" title="Theme picker">
  <svg
    bind:this={svgEl}
    viewBox="0 0 100 100"
    width="54"
    height="54"
    class="clock"
    class:clock-dragging={dragging}
    class:clock-live={mode === "wall"}
    role="group"
    aria-label="Theme clock — drag or click an hour to switch themes"
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
    onpointercancel={handlePointerUp}
  >
    <title>Theme clock — drag or click an hour to switch themes</title>
    <circle cx="50" cy="50" r="45" class="clock-face" />
    <g
      class="clock-hand-group"
      class:clock-hand-no-transition={dragging}
      style="transform: rotate({currentAngle}deg); transform-origin: 50px 50px"
    >
      <line x1="50" y1="50" x2="50" y2="20" class="clock-hand" />
    </g>
    <circle cx="50" cy="50" r="3" class="clock-center" />

    {#each THEMES as theme (theme.hour)}
      {@const pos = hourPosition(theme.hour, CX, CY, DOT_RADIUS_OUTER)}
      {@const isActive = theme.name === $currentTheme}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <g
        role="button"
        tabindex="0"
        class="clock-hour"
        aria-label="{theme.name} — {theme.desc}"
        data-theme-name={theme.name}
        data-theme-desc={theme.desc}
        onclick={() => selectTheme(theme.name)}
        onkeydown={(e) => handleKeydown(e, theme.name)}
      >
        <rect
          x={pos.x - HIT_SIZE / 2}
          y={pos.y - HIT_SIZE / 2}
          width={HIT_SIZE}
          height={HIT_SIZE}
          fill="transparent"
          class="clock-hit"
        />
        <circle
          cx={pos.x}
          cy={pos.y}
          r={isActive ? DOT_R_ACTIVE : DOT_R_INACTIVE}
          class="clock-dot"
          class:clock-dot-active={isActive}
        />
      </g>
    {/each}
  </svg>

  <button
    class="clock-mode-toggle"
    title={mode === "wall" ? "Following time of day" : "Manual theme"}
    aria-label={mode === "wall"
      ? "Switch to manual theme"
      : "Follow time of day"}
    onclick={toggleMode}
  >
    {#if mode === "wall"}
      <svg viewBox="0 0 16 16" width="14" height="14" class="clock-mode-icon">
        <circle cx="8" cy="8" r="2.5" fill="currentColor" />
        {#each sunRays as [x1, y1, x2, y2]}
          <line
            {x1}
            {y1}
            {x2}
            {y2}
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
          />
        {/each}
      </svg>
    {:else}
      <svg viewBox="0 0 16 16" width="14" height="14" class="clock-mode-icon">
        <line
          x1="2"
          y1="14"
          x2="9.5"
          y2="6.5"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        />
        <line
          x1="12"
          y1="1.5"
          x2="12"
          y2="6.5"
          stroke="currentColor"
          stroke-width="1.2"
          stroke-linecap="round"
        />
        <line
          x1="9.5"
          y1="4"
          x2="14.5"
          y2="4"
          stroke="currentColor"
          stroke-width="1.2"
          stroke-linecap="round"
        />
        <circle cx="10" cy="2" r="0.8" fill="currentColor" />
        <circle cx="14.2" cy="6" r="0.8" fill="currentColor" />
      </svg>
    {/if}
  </button>
</div>
