use std::path::PathBuf;

const PLACEHOLDER_INDEX: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>pizza-delivery</title></head><body><div id=\"root\">web UI not built; run bun run build in web/</div></body></html>";

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("web");
    let dist = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let _ = std::fs::create_dir_all(&out);
    for (name, fallback) in [
        ("index.html", PLACEHOLDER_INDEX),
        ("app.js", ""),
        ("app.css", ""),
    ] {
        let bytes = std::fs::read(dist.join(name)).unwrap_or_else(|_| fallback.as_bytes().to_vec());
        let _ = std::fs::write(out.join(name), bytes);
    }
}
