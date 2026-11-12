import { expect, test } from "bun:test";
import { brotliDecompressSync, gunzipSync } from "node:zlib";
import { buildId, compress, renderIndex, renderWorker } from "./release";

const WORKER = `const BUILD = "dev"; // @build
const ASSETS = []; // @assets
self.addEventListener("fetch", () => {});
`;

test("the index references the content-hashed script and stylesheet", () => {
  const html = renderIndex({ script: "app-1a2b3c4d.js", style: "app-5e6f7a8b.css" });
  expect(html).toContain('<link rel="stylesheet" href="/app-5e6f7a8b.css" />');
  expect(html).toContain('<script type="module" src="/app-1a2b3c4d.js"></script>');
  expect(html).not.toContain("/app.js");
});

test("the worker gets the build id and the hashed assets to precache", () => {
  const out = renderWorker(WORKER, "abc123", ["app-1.js", "app-2.css"]);
  expect(out).toContain('const BUILD = "abc123";');
  expect(out).toContain('const ASSETS = ["/app-1.js","/app-2.css"];');
  expect(out).toContain('addEventListener("fetch"');
});

test("a worker without the markers is rejected instead of shipping stale", () => {
  expect(() => renderWorker("const x = 1;", "abc", ["app-1.js"])).toThrow(/marker/);
});

test("the build id changes with any file content and not with order", () => {
  const a = buildId([
    ["a.js", "one"],
    ["b.css", "two"],
  ]);
  expect(
    buildId([
      ["b.css", "two"],
      ["a.js", "one"],
    ]),
  ).toBe(a);
  expect(
    buildId([
      ["a.js", "one!"],
      ["b.css", "two"],
    ]),
  ).not.toBe(a);
  expect(a).toMatch(/^[0-9a-f]{12}$/);
});

test("compress yields brotli and gzip that round-trip and are smaller", () => {
  const text = new TextEncoder().encode("body{color:red}".repeat(200));
  const out = compress(text);
  expect(out).not.toBeNull();
  expect(new Uint8Array(brotliDecompressSync(out!.br))).toEqual(text);
  expect(new Uint8Array(gunzipSync(out!.gz))).toEqual(text);
  expect(out!.br.length).toBeLessThan(text.length);
});

test("tiny files are left alone", () => {
  expect(compress(new TextEncoder().encode("<svg/>"))).toBeNull();
});
