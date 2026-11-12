import { createHash } from "node:crypto";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

export function renderIndex(files: { script: string; style: string }): string {
  return `<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>pizza-delivery</title>
    <meta name="theme-color" content="#0f1b2a" />
    <link rel="manifest" href="/manifest.webmanifest" crossorigin="use-credentials" />
    <link rel="icon" href="/icon.svg" type="image/svg+xml" />
    <link rel="apple-touch-icon" href="/icon-192.png" />
    <link rel="stylesheet" href="/${files.style}" />
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/${files.script}"></script>
  </body>
</html>
`;
}

// The worker in public/ is valid JS on its own (dev defaults); the build swaps the two
// marked lines so the cache name and precache list follow the hashed bundle.
export function renderWorker(source: string, build: string, assets: string[]): string {
  const withBuild = source.replace(
    /^const BUILD = .*; \/\/ @build$/m,
    `const BUILD = ${JSON.stringify(build)};`,
  );
  const out = withBuild.replace(
    /^const ASSETS = .*; \/\/ @assets$/m,
    `const ASSETS = ${JSON.stringify(assets.map((a) => `/${a}`))};`,
  );
  if (out === source || out === withBuild) {
    throw new Error("sw.js is missing the @build/@assets marker lines");
  }
  return out;
}

export function buildId(files: Array<[string, string | Uint8Array]>): string {
  const hash = createHash("sha256");
  for (const [name, bytes] of [...files].sort(([a], [b]) => a.localeCompare(b))) {
    hash.update(name);
    hash.update(bytes);
  }
  return hash.digest("hex").slice(0, 12);
}

export const COMPRESSIBLE = [".html", ".js", ".css", ".svg", ".webmanifest", ".json"];
const MIN_BYTES = 256;

export function compress(bytes: Uint8Array): { br: Uint8Array; gz: Uint8Array } | null {
  if (bytes.length < MIN_BYTES) return null;
  const br = brotliCompressSync(bytes, {
    params: {
      [constants.BROTLI_PARAM_QUALITY]: 11,
      [constants.BROTLI_PARAM_SIZE_HINT]: bytes.length,
    },
  });
  const gz = gzipSync(bytes, { level: 9 });
  return br.length < bytes.length ? { br, gz } : null;
}
