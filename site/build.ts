import { rmSync, mkdirSync, copyFileSync, readFileSync, writeFileSync } from "fs";
import path from "path";
import { renderBrandBook } from "./src/brand-render";

const ROOT = import.meta.dir;
const DIST = path.join(ROOT, "dist");
const UI = path.join(ROOT, "..", "crates", "ox-web", "ui");
const WASM_PKG = path.join(ROOT, "..", "target", "wasm-pkg");

// 1. Clean dist/
rmSync(DIST, { recursive: true, force: true });
mkdirSync(DIST, { recursive: true });
mkdirSync(path.join(DIST, "fonts"), { recursive: true });

// 2. Bundle src/main.ts → dist/main.js
const result = await Bun.build({
  entrypoints: [path.join(ROOT, "src", "main.ts")],
  outdir: DIST,
  naming: "[name].[ext]",
  target: "browser",
  format: "esm",
  minify: true,
});

if (!result.success) {
  console.error("Build failed:");
  for (const log of result.logs) console.error(log);
  process.exit(1);
}
console.log("Bundled main.js");

// 3. Copy main.css → dist/theme.css with font path rewrite
const mainCss = readFileSync(path.join(UI, "styles", "main.css"), "utf-8");
const themeCss = mainCss.replaceAll("./fonts/", "/fonts/");
writeFileSync(path.join(DIST, "theme.css"), themeCss);
console.log("Wrote theme.css");

// 4. Copy site.css
copyFileSync(
  path.join(ROOT, "styles", "site.css"),
  path.join(DIST, "site.css"),
);
console.log("Copied site.css");

// 5. Copy HTML pages + generate brand book
copyFileSync(path.join(ROOT, "index.html"), path.join(DIST, "index.html"));
const brandMd = readFileSync(path.join(ROOT, "..", "BRAND_BOOK.md"), "utf-8");
writeFileSync(path.join(DIST, "brand.html"), renderBrandBook(brandMd));
console.log("Copied index.html, generated brand.html");

// 6. Copy font files
const fonts = [
  "manrope-latin.woff2",
  "outfit-latin.woff2",
  "ibm-plex-mono-400-latin.woff2",
  "ibm-plex-mono-500-latin.woff2",
  "ibm-plex-mono-600-latin.woff2",
];
for (const font of fonts) {
  copyFileSync(
    path.join(UI, "fonts", font),
    path.join(DIST, "fonts", font),
  );
}
console.log(`Copied ${fonts.length} font files`);

// 7. Build playground (flat structure: playground/{index.html,main.js,main.css,fonts/,pkg/})
console.log("\nBuilding playground...");
const PG = path.join(DIST, "playground");
mkdirSync(path.join(PG, "pkg"), { recursive: true });
mkdirSync(path.join(PG, "fonts"), { recursive: true });

// 7a. Copy playground index.html
copyFileSync(
  path.join(ROOT, "..", "crates", "ox-web", "static", "index.html"),
  path.join(PG, "index.html"),
);

// 7b. Copy wasm-pack output
for (const file of ["ox_web_bg.wasm", "ox_web.js", "ox_web.d.ts"]) {
  copyFileSync(path.join(WASM_PKG, file), path.join(PG, "pkg", file));
}
console.log("Copied wasm-pack output");

// 7c. Bundle playground UI
const pgResult = await Bun.build({
  entrypoints: [path.join(UI, "src", "main.ts")],
  outdir: PG,
  naming: "[name].[ext]",
  target: "browser",
  format: "esm",
  minify: true,
  external: ["/pkg/*"],
});

if (!pgResult.success) {
  console.error("Playground JS build failed:");
  for (const log of pgResult.logs) console.error(log);
  process.exit(1);
}

// Rewrite absolute /pkg/ paths to relative ./pkg/ so playground works under /playground/
const pgMainJs = path.join(PG, "main.js");
const pgJs = readFileSync(pgMainJs, "utf-8");
writeFileSync(pgMainJs, pgJs.replaceAll("/pkg/", "./pkg/"));
console.log("Bundled playground main.js");

// 7d. Copy playground CSS (font paths are already relative ./fonts/)
copyFileSync(
  path.join(UI, "styles", "main.css"),
  path.join(PG, "main.css"),
);

// 7e. Copy fonts into playground
for (const font of fonts) {
  copyFileSync(
    path.join(UI, "fonts", font),
    path.join(PG, "fonts", font),
  );
}
console.log("Playground build complete");

console.log("\nBuild complete → site/dist/");
