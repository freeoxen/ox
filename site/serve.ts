import path from "path";

const DIST = path.join(import.meta.dir, "dist");

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".woff2": "font/woff2",
  ".wasm": "application/wasm",
};

const server = Bun.serve({
  port: 0,
  async fetch(req) {
    let pathname = new URL(req.url).pathname;
    if (pathname.endsWith("/")) pathname += "index.html";

    const file = Bun.file(path.join(DIST, pathname));
    if (!(await file.exists())) {
      return new Response("Not Found", { status: 404 });
    }

    const ext = path.extname(pathname);
    const headers: Record<string, string> = { "Cache-Control": "no-store" };
    if (MIME[ext]) headers["Content-Type"] = MIME[ext];

    return new Response(file, { headers });
  },
});

console.log(server.url.href);
