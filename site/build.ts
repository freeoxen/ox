import {
  rmSync,
  mkdirSync,
  copyFileSync,
  readFileSync,
  writeFileSync,
  readdirSync,
  statSync,
} from "fs";
import path from "path";
import { renderBrandBook } from "./src/brand-render";

const ROOT = import.meta.dir;
const DIST = path.join(ROOT, "dist");
const UI = path.join(ROOT, "..", "crates", "ox-web", "ui");
const WASM_PKG = path.join(ROOT, "..", "target", "wasm-pkg");
const JS_PKG = path.join(ROOT, "..", "target", "js-pkg");

// Recursive directory copy
function copyDirSync(src: string, dest: string) {
  mkdirSync(dest, { recursive: true });
  for (const entry of readdirSync(src)) {
    const srcPath = path.join(src, entry);
    const destPath = path.join(dest, entry);
    if (statSync(srcPath).isDirectory()) {
      copyDirSync(srcPath, destPath);
    } else {
      copyFileSync(srcPath, destPath);
    }
  }
}

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

// 3. Copy app.css → dist/theme.css with font path rewrite
const appCss = readFileSync(path.join(UI, "src", "app.css"), "utf-8");
const themeCss = appCss.replaceAll("/fonts/", "/fonts/");
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
    path.join(UI, "static", "fonts", font),
    path.join(DIST, "fonts", font),
  );
}
console.log(`Copied ${fonts.length} font files`);

// 7. Build playground — copy SvelteKit build output + wasm pkg
console.log("\nBuilding playground...");
const PG = path.join(DIST, "playground");

// 7a. Copy SvelteKit build output (index.html, _app/, fonts/)
copyDirSync(JS_PKG, PG);

// 7b. Rewrite absolute paths to relative so playground works under /playground/
const pgHtmlPath = path.join(PG, "index.html");
let pgHtml = readFileSync(pgHtmlPath, "utf-8");
// Static asset references in HTML
pgHtml = pgHtml.replaceAll('"/_app/', '"./_app/');
pgHtml = pgHtml.replaceAll('"/fonts/', '"./fonts/');
pgHtml = pgHtml.replaceAll('"/pkg/', '"./pkg/');
// Dynamic import() calls in inline script
pgHtml = pgHtml.replaceAll('("/_app/', '("./_app/');
// Set SvelteKit base path for runtime resolution (version.json, etc.)
pgHtml = pgHtml.replace('base: ""', 'base: "/playground"');
writeFileSync(pgHtmlPath, pgHtml);

// 7c. Copy wasm-pack output into playground/pkg/
mkdirSync(path.join(PG, "pkg"), { recursive: true });
for (const file of ["ox_web_bg.wasm", "ox_web.js", "ox_web.d.ts"]) {
  copyFileSync(path.join(WASM_PKG, file), path.join(PG, "pkg", file));
}
console.log("Copied wasm-pack output");

// 7d. Copy fonts into playground (if not already present from SvelteKit build)
mkdirSync(path.join(PG, "fonts"), { recursive: true });
for (const font of fonts) {
  const dest = path.join(PG, "fonts", font);
  try {
    statSync(dest);
  } catch {
    copyFileSync(path.join(UI, "static", "fonts", font), dest);
  }
}
console.log("Playground build complete");

console.log("\nBuild complete → site/dist/");
