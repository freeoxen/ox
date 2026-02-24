import { rmSync, mkdirSync, copyFileSync, readFileSync, writeFileSync } from "fs";
import path from "path";
import { renderBrandBook } from "./src/brand-render";

const ROOT = import.meta.dir;
const DIST = path.join(ROOT, "dist");
const UI = path.join(ROOT, "..", "crates", "ox-web", "ui");

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
const themeCss = mainCss.replaceAll("/dist/fonts/", "/fonts/");
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

console.log("\nBuild complete → site/dist/");
