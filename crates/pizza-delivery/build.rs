use std::path::{Path, PathBuf};

const PLACEHOLDER_INDEX: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>pizza-delivery</title></head><body><div id=\"root\">web UI not built; run bun run build in web/</div></body></html>";

const BUILT: [(&str, &str); 3] = [
    ("index.html", PLACEHOLDER_INDEX),
    ("app.js", ""),
    ("app.css", ""),
];

const STATIC: [&str; 5] = [
    "manifest.webmanifest",
    "sw.js",
    "icon.svg",
    "icon-192.png",
    "icon-512.png",
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("web");
    let workspace_web = manifest.join("../../web");
    let vendored = manifest.join("web-assets");
    let dist = workspace_web.join("dist");
    let public = workspace_web.join("public");
    for dir in [&dist, &public, &vendored] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    let _ = std::fs::create_dir_all(&out);
    for (name, fallback) in BUILT {
        write_first(&out, name, &[&dist, &vendored], fallback.as_bytes());
    }
    for name in STATIC {
        write_first(&out, name, &[&public, &vendored], &[]);
    }
}

fn write_first(out: &Path, name: &str, dirs: &[&Path], fallback: &[u8]) {
    let bytes = dirs
        .iter()
        .find_map(|dir| std::fs::read(dir.join(name)).ok())
        .unwrap_or_else(|| fallback.to_vec());
    let _ = std::fs::write(out.join(name), bytes);
}
