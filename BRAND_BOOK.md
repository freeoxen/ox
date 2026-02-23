# ox — Brand Book

**v1.0 — February 2026 — AdjectiveNoun**

The ox is the oldest working animal. It does not sprint. It does not perform. It lowers its head, leans into the yoke, and pulls — across any terrain, in any weather, for as long as the work demands. It is patient, grounded, and immovably strong.

This brand identity is built in that image. A visual language of landscapes and earth — the terrain the ox walks. Mid-century modern poster art rendered through flat vector minimalism. Broad, solid, warm. Every surface is opaque. Every form is deliberate. Nothing is ornamental. Nothing is wasted.

Five colors. Layered silhouettes. Infinite depth through honest weight.

---

## 01 — Essence

The ox does not decorate. It works.

### Steady Ground, Not Gloss

The ox stands on solid earth. UI elements should feel the same way — geological layers, overlapping, opaque, organic. Depth comes from stacking flat shapes, the way terrain stacks against the horizon. Never from shadows, gradients, or chrome. The interface is ground you stand on, not glass you look through.

### Five Colors, No More

An ox is not a peacock. The palette is five values and absolutely nothing else. Deep indigo, warm amber, vermillion, white, beige. No grays. No tints. No opacity tricks that produce off-palette colors. The ox carries what it needs and nothing extra. Constraint is strength.

### Poster Clarity

The ox is legible from any distance — broad shoulders, unmistakable silhouette. Every screen should have that same immediate readability: bold forms, clear hierarchy, a sense of place. Like a mid-century travel poster, the composition does the work. No filigree. No footnotes.

---

## 02 — Color Palette

Five colors. The earth the ox walks, the sky it works under, the sun that warms its back.

| Name            | Hex       | CSS Variable     | Role                                              |
|-----------------|-----------|------------------|---------------------------------------------------|
| Deep Indigo     | `#2B2D7C` | `--indigo`       | The bedrock. Backgrounds, deep surfaces, text, receding layers. Everything rests on it the way weight rests on the ox's shoulders. |
| Warm Amber      | `#F5A623` | `--amber`        | The sun overhead. Interactive elements, highlights, active states, progress indicators. Warm and directional — it draws the eye where the work is. |
| Vermillion      | `#E8471B` | `--vermillion`   | The brand on the flank. Errors, destructive actions, critical badges, emphasis. Used sparingly. A mark that means something because it's rare. |
| White           | `#FFFFFF` | `--white`        | Open sky. Cards, panels, input fields, clouds. Clear, clean space where content breathes. |
| Warm Beige      | `#E8D5C0` | `--beige`        | Dry earth. Page background, subtle borders, secondary surfaces. The ground between features — warm, neutral, endless. |

### Functional Aliases

```css
--bg-primary:    var(--beige);
--bg-deep:       var(--indigo);
--bg-surface:    var(--white);
--text-primary:  var(--indigo);
--text-inverse:  var(--beige);
--accent-warm:   var(--amber);
--accent-hot:    var(--vermillion);
```

### Color Rules

- No gradients, ever. The ox is solid through and through. Flat fills only.
- No opacity to blend colors. Each surface is one of the five, fully opaque. No half-measures.
- No derived shades (lighter indigo, darker beige). The five are the five. The ox does not carry optional accessories.
- Depth comes from overlapping shapes, the way hills overlap at the horizon. Not from color manipulation.
- Text on indigo/vermillion: beige or white.
- Text on beige/white/amber: indigo.

### Application Modes

These are not "light mode" and "dark mode." The ox works at all hours. They are **Day** and **Dusk**.

**Day (Amber Dominant):** Beige canvas, white content surfaces, amber accents, indigo text and buttons. The ox under a warm sun. Use for primary UI states, content-heavy screens, editing workflows.

**Dusk (Indigo Dominant):** Indigo canvas, amber-bordered content surfaces, vermillion and amber accents, beige text. The ox still pulling as the sky deepens. Use for focused states, immersive views, the chat experience when in active conversation.

---

## 03 — Typography

Three faces. Sturdy, legible, purposeful. Like the ox, each typeface has a clear job and does it without complaint.

### Syne — Display / Headlines

The ox's bellow. Heavy, geometric, unmistakable from a distance.

**Use for:** All headlines, section titles, brand marks, buttons, card headers.

| Weight    | Value | Usage                     |
|-----------|-------|---------------------------|
| Regular   | 400   | Subtle display text       |
| SemiBold  | 600   | UI labels, buttons        |
| Bold      | 700   | Section titles, card heads|
| ExtraBold | 800   | Hero display, brand mark  |

Tight letter-spacing at large sizes (`-0.03em`). Line-height `0.95–1.1` for display.

### Outfit — Body / Prose

The ox's steady breathing. Warm, humanist, comfortable at reading distance.

**Use for:** Body text, descriptions, chat messages, secondary labels.

| Weight    | Value | Usage                          |
|-----------|-------|--------------------------------|
| Light     | 300   | Large lead paragraphs          |
| Regular   | 400   | Body text                      |
| Medium    | 500   | Emphasis within body            |
| SemiBold  | 600   | Strong inline emphasis          |

Line-height `1.6` for readability. No letter-spacing adjustment needed.

### IBM Plex Mono — Code / Metadata

The ox's harness — functional, precise, engineered for load-bearing.

**Use for:** Code blocks, tool definitions, technical metadata, badges, timestamps, small utility labels.

Weight 400 for code, 500–600 for labels. Size `0.65–0.85rem` depending on context. Letter-spacing `+0.05–0.1em` for uppercase labels.

### Type Scale

Fluid, modular scale using `clamp()`. Ratio approximately 1.333 (perfect fourth).

```css
--step-0: clamp(1rem, 0.95rem + 0.25vw, 1.125rem);     /* Body text */
--step-1: clamp(1.25rem, 1.15rem + 0.5vw, 1.5rem);     /* Lead text, subheads */
--step-2: clamp(1.5rem, 1.3rem + 1vw, 2rem);            /* Section subheads */
--step-3: clamp(2rem, 1.6rem + 2vw, 3rem);              /* Section titles */
--step-4: clamp(2.5rem, 1.8rem + 3.5vw, 4.5rem);       /* Page titles */
--step-5: clamp(3rem, 2rem + 5vw, 7rem);                /* Hero / brand display */
```

### Font Loading

```html
<link href="https://fonts.googleapis.com/css2?family=Syne:wght@400;500;600;700;800&family=Outfit:wght@300;400;500;600&family=IBM+Plex+Mono:wght@400;500;600&display=swap" rel="stylesheet">
```

---

## 04 — Illustration Style

The ox's world. Broad valleys, layered ridgelines, the long walk from horizon to horizon. Every illustration is the terrain the ox inhabits — rendered in mid-century modern poster art through contemporary flat vector.

### Core Style Preamble

Include this as a base style description for all illustration generation:

> Flat vector minimalist landscape illustration. Strictly limited palette of exactly 5 colors: deep indigo (#2B2D7C), warm amber-orange (#F5A623), vermillion red-orange (#E8471B), white (#FFFFFF), and warm beige (#E8D5C0). No gradients, no outlines, no textures. All forms are smooth opaque silhouettes with organic curves. Simple geometric sun/moon circle. Pill-shaped white clouds. Layered overlapping shapes to create depth. Style of mid-century modern poster art meets contemporary flat vector illustration.

### Shape Language

The ox is all curve and mass. No sharp edges except where the land itself breaks — a mountain peak, a canyon rim.

- **Terrain:** Organic smooth curves, rounded silhouettes. No sharp corners except occasional geometric mountain peaks.
- **Celestials:** Sun and moon are simple circles — the way an ox sees them, unhurried, overhead. No rays, no glows, no halos.
- **Clouds:** Pill-shaped (stadium geometry). `border-radius: 999px` in CSS. Drifting, patient, like the ox itself.
- **Water:** Sinuous ribbons of white or beige cutting through terrain layers. The rivers the ox fords.
- **Trees:** Rounded elliptical canopy shapes, overlapping to form forests. Shade for the long rest.

### Depth Method

Depth is created **only** through overlapping opaque flat shapes. Darker values (indigo) recede; warmer values (amber, vermillion) advance. No gradients. No atmospheric perspective. No shadows. Honest depth — the kind you get from walking the land, not from tricks of light.

### Composition Variables

The ox crosses every kind of ground. Cycle through these to generate variety while keeping the style locked:

| Variable         | Options                                                        |
|------------------|----------------------------------------------------------------|
| Time of day      | Amber-dominant (day) vs. indigo-dominant (night/dusk)          |
| Terrain type     | Coastal, mountain, desert, valley, river, volcanic, plains, canyon, archipelago |
| Composition      | Panoramic horizontal layers vs. vertical dramatic feature vs. close-up detail |
| Sun/moon         | High center, low horizon, clipped by terrain, hidden behind clouds |
| Cloud behavior   | Contained within frame vs. breaking/bleeding past the edge     |

### Scene Prompts

Each prompt should be prefixed with the Core Style Preamble above.

**Coastal / Ocean:**
A sweeping coastal scene. Dark indigo ocean with a large curving wave in the foreground. Orange-amber sand dunes along the shore. A red-orange setting sun sits low on the horizon, partially clipped by a rolling hill. Two white pill-shaped clouds float above. Beige sky background.

**Desert Canyon:**
A deep canyon viewed from above. Layered indigo shadow walls recede into depth. Amber and red-orange sunlit cliff faces in the foreground. A winding white river cuts through the canyon floor. Small circular orange sun high in an indigo sky. One cloud breaks the frame edge.

**Alpine / Snow Peaks:**
Jagged mountain peaks in indigo and red-orange, overlapping in layers. White snow caps on the tallest peaks. A large amber-orange sun behind the mountains casting the sky in warm tones. Smooth rolling indigo foothills in the foreground. Clouds drifting across the mid-ground.

**Rolling Hills at Dusk:**
Soft undulating hills layered front to back. Foreground hills in deep indigo, mid-ground in red-orange, background hills in amber. A white moon circle in the indigo sky. Three white clouds scattered across the composition. A sinuous dark river cutting between two hills.

**Volcanic / Dramatic:**
A single tall volcanic peak in red-orange with an indigo base. Amber lava glow at the summit. Dark indigo sky. Layered indigo foothills at the base. Clouds wrapping around the peak, spilling beyond the frame. Small white moon in the upper corner.

**Forest Valley:**
Rounded tree canopy shapes in indigo and red-orange filling a valley. Amber sky above. A gap in the trees reveals a winding white path. Circular orange sun peeking through the canopy. Soft cloud shapes along the top edge.

### Generation Parameters

When generating illustrations with any tool, reinforce:

| Attribute          | Specification                                                                   |
|--------------------|---------------------------------------------------------------------------------|
| Color strictness   | "Use ONLY these 5 hex colors: #2B2D7C, #F5A623, #E8471B, #FFFFFF, #E8D5C0. No other colors." |
| No gradients       | "Completely flat fills, no gradients, no shading, no soft shadows"               |
| No outlines        | "No strokes, no outlines, no borders on shapes"                                  |
| Shape language     | "Organic smooth curves, rounded silhouettes, no sharp corners except occasional geometric peaks" |
| Depth method       | "Depth created only through overlapping opaque flat shapes, darker values recede" |
| Aspect ratio       | Square (1:1) for tiles; widescreen for banners                                   |
| Negative prompts   | "No photorealism, no 3D rendering, no texture, no noise, no halftone, no line art, no watercolor, no pencil" |

### Do / Never

**Do:**
- Use only the five palette colors, fully opaque
- Create depth with overlapping flat shapes
- Use organic curves and smooth silhouettes
- Keep sun/moon as simple circles
- Make clouds pill-shaped
- Let elements bleed past the frame edge
- Vary time-of-day and terrain type

**Never:**
- Use gradients, shading, or soft shadows
- Add outlines, strokes, or borders on illustration shapes
- Use texture, noise, halftone, or grain
- Introduce colors outside the five
- Use photorealism, 3D rendering, or watercolor effects
- Add line art, pencil marks, or hand-drawn effects
- Use opacity to create blended/derived colors
- Add sun rays, glows, lens flares, or atmospheric haze

---

## 05 — UI Components

The ox's gear is simple, functional, and built to last. No decorative stitching, no polished buckles. Every component earns its place through use. Flat, opaque, layered. No rounded corners on rectangles — the only curves belong to organic forms, the way the ox's body curves but its yoke does not.

### Buttons

```css
.btn {
  font-family: 'Syne', sans-serif;
  font-weight: 600;
  font-size: 0.9rem;
  letter-spacing: 0.02em;
  border: none;
  cursor: pointer;
  padding: 0.7em 1.8em;
  transition: transform 0.2s;
}

.btn:hover  { transform: translateY(-2px); }
.btn:active { transform: translateY(0); }
```

| Variant   | Background     | Text Color | Usage                        |
|-----------|----------------|------------|------------------------------|
| Primary   | `--vermillion` | `--white`  | Main actions (Send, Submit). The brand on the flank — it means *go*. |
| Secondary | `--indigo`     | `--beige`  | Secondary actions (Cancel). Solid bedrock beneath the primary. |
| Accent    | `--amber`      | `--indigo` | Constructive actions (New Tool). Warm, inviting, productive. |
| Ghost     | transparent    | `--indigo` | Tertiary actions (Details, Expand). Present but unobtrusive. |

Small variant: `font-size: 0.75rem; padding: 0.5em 1.2em;`

No border-radius on any button. Sharp rectangular geometry. The yoke is square-hewn.

### Inputs

```css
.ox-input {
  font-family: 'Outfit', sans-serif;
  font-size: 0.95rem;
  padding: 0.7em 1em;
  background: var(--white);
  color: var(--indigo);
  border: 2px solid var(--beige);
  outline: none;
  transition: border-color 0.3s;
}

.ox-input:focus {
  border-color: var(--amber);
}

.ox-input::placeholder {
  color: var(--indigo);
  opacity: 0.3;
}
```

When focused, the border shifts to amber — the sun falling on the place where work happens. Textareas use IBM Plex Mono at `0.85rem`. Labels use IBM Plex Mono at `0.75rem`, uppercase, `letter-spacing: 0.08em`, color `--indigo` at 50% opacity.

### Cards

```css
.ox-card {
  background: var(--white);
  padding: var(--space-md);
  position: relative;
  transition: transform 0.3s cubic-bezier(0.22, 1, 0.36, 1);
}

.ox-card:hover {
  transform: translateY(-4px);
}
```

Cards have a 4px bottom stripe — a thin band of earth color identifying what the card carries:
- `--amber` for tools and active items
- `--indigo` for system/configuration items
- `--vermillion` for errors and alerts

Card tags use IBM Plex Mono, `0.65rem`, uppercase, `letter-spacing: 0.1em`, color `--vermillion`.

### Badges

```css
.ox-badge {
  font-family: 'IBM Plex Mono', monospace;
  font-size: 0.65rem;
  font-weight: 600;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  padding: 0.3em 0.8em;
}
```

| Variant    | Background     | Text       |
|------------|----------------|------------|
| Indigo     | `--indigo`     | `--beige`  |
| Amber      | `--amber`      | `--indigo` |
| Vermillion | `--vermillion` | `--white`  |
| Ghost      | transparent    | `--indigo` (with 1.5px indigo border) |

### Status Indicators

A pulsing dot (8px circle) with a monospace label. The ox's heartbeat.

- **Active:** Amber dot, `opacity: 0.3 <> 1.0` over `1.2s ease-in-out infinite`. The ox is pulling. Steady rhythm.
- **Idle:** Indigo dot at 30% opacity, static. The ox is resting. Still present.
- **Error:** Vermillion dot, static. Something has stopped the work.

### Chat Messages

Messages are separated by 1px beige borders — thin lines of earth between strata of conversation. Each message has a role label above the body.

| Role         | Label Color    | Label Format                         |
|--------------|----------------|--------------------------------------|
| system       | `--indigo` @ 35% opacity | IBM Plex Mono, 0.65rem, uppercase |
| user         | `--indigo`     | IBM Plex Mono, 0.65rem, uppercase    |
| assistant    | `--amber`      | IBM Plex Mono, 0.65rem, uppercase    |
| tool_result  | `--vermillion` | IBM Plex Mono, 0.65rem, uppercase    |

**Tool call blocks:** Beige background, IBM Plex Mono `0.8rem`, indigo text. Prefixed with `->`. The ox reaching for a tool.

**Inline code:** IBM Plex Mono `0.85em`, beige background, `padding: 0.1em 0.4em`.

---

## 06 — Layout

The ox sees the world as open terrain with features on the horizon. The interface follows: a broad field for conversation, a ridgeline of debug panels along the right edge. Borders are beige — dry earth between the layers.

### Structure

```
+-------------------------------------------------------------+
|                        HEADER: ox                            |
+------------------------------------------+------------------+
|                                          |    CONTEXT       |
|    Chat                                  |    DEBUGGER      |
|    (scrollable, max-width 720px)         |                  |
|                                          +------------------+
|                                          |    TOOLS         |
|                                          |    PANEL         |
|    [Status Indicator]                    +------------------+
|    [Input Field] [Send]                  |    REQUEST       |
|                                          |    LOG           |
+------------------------------------------+------------------+
```

- **Chat column:** `flex: 1`, max-width `720px` for readability. The open pasture — room to think.
- **Debug column:** Fixed `420px`, contains three collapsible panels. The fence line — structure at the boundary.
- **Gap between columns:** `2px` beige. Dry earth between fields.
- **Active panel indicator:** `3px` amber left border. Sunlight marking the panel under attention.

### Spacing System

Geometric progression. No arbitrary values. The ox's stride is even and predictable.

| Token        | Value   | px  | Usage                              |
|--------------|---------|-----|------------------------------------|
| `--space-xs` | 0.5rem  | 8   | Tight gaps, inline spacing         |
| `--space-sm` | 1rem    | 16  | Component padding, list gaps       |
| `--space-md` | 2rem    | 32  | Card padding, grid gaps            |
| `--space-lg` | 4rem    | 64  | Section padding                    |
| `--space-xl` | 6rem    | 96  | Major section spacing              |
| `--space-2xl`| 10rem   | 160 | Hero/footer vertical padding       |

### Borders and Dividers

- **Beige** (`2px`): Subtle dividers between same-surface elements. Furrows in the field.
- **Amber** (`2px`): Active/focused borders, selected panel indicators. Where the sun falls.
- **Indigo** (`3px`): Strong structural dividers, section breaks. The ridgeline.
- **Vermillion** (`2px`): Error state borders only. The brand mark — only when it matters.

No rounded borders. Sharp geometry for all rectangular UI elements. The ox's world has round hills and square fences.

---

## 07 — Motion

The ox does not rush. Movement is tectonic — slow, weighted, inevitable. Things rise into place the way hills emerge at dawn. Things settle the way dust settles after the plow passes.

### Easing Functions

| Name   | Value                              | Usage                         |
|--------|------------------------------------|-------------------------------|
| Enter  | `cubic-bezier(0.22, 1, 0.36, 1)`  | Elements entering the viewport. The long approach. |
| Spring | `cubic-bezier(0.34, 1.56, 0.64, 1)` | Interactive feedback. The head toss. |
| Pulse  | `ease-in-out`                      | Status indicators. The heartbeat. |

### Patterns

**Enter:** Elements rise from below with fade. `translateY(20px) -> 0`, `opacity: 0 -> 1`. Duration `0.8–1.2s`. Stagger siblings by `0.15–0.3s` using `animation-delay`. Terrain emerging at daybreak.

**Hover:** `translateY(-2px)` to `(-4px)`. Duration `0.2–0.3s`. Cards lift, buttons lift. The slight shift of weight before the step.

**Active / Press:** `translateY(0)` — snap back. Immediate. Hoof meeting earth.

**Status pulse:** `opacity: 0.3 <> 1.0` over `1.2s`. For active process indicators only. The steady breath.

**Expand / Collapse:** Height transition with `max-height`. `0.4s ease-out`. The slow nod.

**Page load:** Stagger key elements on initial render. Landscape layers emerge bottom-up with `0.3s` stagger. Content fades up after the terrain settles. The ox cresting a hill — first the land appears, then the sky, then the path forward.

---

## 08 — Iconography

Icons are the brands and trail markers of the ox's world. Flat filled silhouettes — no strokes, no outlines. Shapes you can read from across the field.

### Icon Metaphors

The ox's landscape maps directly to interface concepts:

| Icon Form            | Meaning           | Color(s)               |
|----------------------|-------------------|------------------------|
| Mountain peak        | Navigation, peaks | Indigo, white snow cap |
| Sun (circle)         | Active, running   | Amber. The working day. |
| Moon (circle)        | Idle, rest        | White. The resting night. |
| Cloud (pill)         | Pending, loading  | White. Patience. |
| River (sinuous path) | Data flow, stream | White or beige. The current that carries. |
| Volcano              | Error, alert      | Vermillion + amber glow. Danger on the path. |
| Forest (ellipses)    | Tools, collection | Indigo + vermillion. Resources gathered. |
| Hills (waves)        | History, timeline | Amber. Ground already covered. |

### Construction Rules

- All icons are filled silhouettes — no strokes, no outlines. Brands, not sketches.
- Use only the five palette colors.
- Prefer simple geometric forms: circles, ellipses, triangles with curved edges. Readable from the far ridge.
- Icons should work at `48px`, `32px`, and `16px`. Simplify at smaller sizes.
- No detail that doesn't read at the smallest target size. If the ox can't see it from the hilltop, it doesn't belong.

---

## 09 — CSS Variable Reference

The full harness. Every token the system needs, nothing it doesn't.

### Colors

```css
:root {
  --indigo:      #2B2D7C;
  --amber:       #F5A623;
  --vermillion:  #E8471B;
  --white:       #FFFFFF;
  --beige:       #E8D5C0;

  --bg-primary:    var(--beige);
  --bg-deep:       var(--indigo);
  --bg-surface:    var(--white);
  --text-primary:  var(--indigo);
  --text-inverse:  var(--beige);
  --accent-warm:   var(--amber);
  --accent-hot:    var(--vermillion);
}
```

### Typography

```css
:root {
  --font-display: 'Syne', sans-serif;
  --font-body:    'Outfit', sans-serif;
  --font-mono:    'IBM Plex Mono', monospace;

  --step-0: clamp(1rem, 0.95rem + 0.25vw, 1.125rem);
  --step-1: clamp(1.25rem, 1.15rem + 0.5vw, 1.5rem);
  --step-2: clamp(1.5rem, 1.3rem + 1vw, 2rem);
  --step-3: clamp(2rem, 1.6rem + 2vw, 3rem);
  --step-4: clamp(2.5rem, 1.8rem + 3.5vw, 4.5rem);
  --step-5: clamp(3rem, 2rem + 5vw, 7rem);
}
```

### Spacing

```css
:root {
  --space-xs:  0.5rem;
  --space-sm:  1rem;
  --space-md:  2rem;
  --space-lg:  4rem;
  --space-xl:  6rem;
  --space-2xl: 10rem;
}
```

### Motion

```css
:root {
  --ease-enter:  cubic-bezier(0.22, 1, 0.36, 1);
  --ease-spring: cubic-bezier(0.34, 1.56, 0.64, 1);
  --ease-pulse:  ease-in-out;
  --duration-enter:   0.8s;
  --duration-hover:   0.2s;
  --duration-pulse:   1.2s;
  --stagger-delay:    0.15s;
}
```

---

*The ox does not explain itself. It walks. The ground remembers.*
