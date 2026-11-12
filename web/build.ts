import { cp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { extname } from "node:path";
import { buildId, COMPRESSIBLE, compress, renderIndex, renderWorker } from "./src/release";

await rm("dist", { recursive: true, force: true });
await mkdir("dist", { recursive: true });

// Content-hashed names let the server mark them immutable; index.html and sw.js are the
// only entry points whose names stay fixed.
const result = await Bun.build({
  entrypoints: ["src/main.tsx"],
  outdir: "dist",
  naming: { entry: "app-[hash].[ext]", asset: "[name]-[hash].[ext]", chunk: "chunk-[hash].js" },
  minify: true,
  target: "browser",
});
if (!result.success) {
  for (const log of result.logs) console.error(log);
  process.exit(1);
}

const built = result.outputs.map((o) => o.path.split("/").pop() ?? "");
const script = built.find((n) => n.startsWith("app-") && n.endsWith(".js"));
const style = built.find((n) => n.startsWith("app-") && n.endsWith(".css"));
if (!script || !style) throw new Error(`bundle lacks a script or stylesheet: ${built.join(", ")}`);

await cp("public", "dist", { recursive: true });

const bundle = built.filter((n) => n.endsWith(".js") || n.endsWith(".css"));
const publicFiles = await readdir("public");
const id = buildId(
  await Promise.all(
    [...bundle.map((n) => [n, `dist/${n}`]), ...publicFiles.map((n) => [n, `public/${n}`])].map(
      async ([name, path]) => [name!, await readFile(path!)] as [string, Uint8Array],
    ),
  ),
);

await writeFile("dist/index.html", renderIndex({ script, style }));
await writeFile("dist/sw.js", renderWorker(await readFile("public/sw.js", "utf8"), id, bundle));

// Precompress so the server never spends CPU on it and the tunnel carries the small bytes.
for (const name of await readdir("dist")) {
  if (!COMPRESSIBLE.includes(extname(name))) continue;
  const packed = compress(await readFile(`dist/${name}`));
  if (!packed) continue;
  await writeFile(`dist/${name}.br`, packed.br);
  await writeFile(`dist/${name}.gz`, packed.gz);
}
