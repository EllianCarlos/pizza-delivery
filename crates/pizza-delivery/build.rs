use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const PLACEHOLDER_INDEX: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>pizza-delivery</title><link rel=\"stylesheet\" href=\"/app.css\"></head><body><div id=\"root\">web UI not built; run bun run build in web/</div><script type=\"module\" src=\"/app.js\"></script></body></html>";

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out_root = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default());
    let out = out_root.join("web");
    let workspace_web = manifest.join("../../web");
    let vendored = manifest.join("web-assets");
    let dist = workspace_web.join("dist");
    let public = workspace_web.join("public");
    for dir in [&dist, &public, &vendored] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::create_dir_all(&out);

    // Earlier directories win: a fresh build, then the vendored release copy, then the
    // static files. Compressed `.br` / `.gz` siblings travel with their base file.
    let mut names: Vec<String> = Vec::new();
    for dir in [&dist, &vendored, &public] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.') && !n.ends_with(".br") && !n.ends_with(".gz"))
            .collect();
        found.sort();
        for name in found {
            if !names.contains(&name) {
                copy_with_variants(dir, &out, &name);
                names.push(name);
            }
        }
    }
    for (name, fallback) in [
        ("index.html", PLACEHOLDER_INDEX),
        ("app.js", ""),
        ("app.css", ""),
    ] {
        if !names.iter().any(|n| n == name) && !has_hashed_bundle(&names, name) {
            let _ = std::fs::write(out.join(name), fallback);
            names.push(name.to_owned());
        }
    }

    let mut table = String::from("pub static ASSETS: &[Asset] = &[\n");
    for name in &names {
        let base = out.join(name);
        let variant = |ext: &str| {
            let path = out.join(format!("{name}.{ext}"));
            if path.is_file() {
                format!("Some(include_bytes!({:?}))", path.display().to_string())
            } else {
                "None".to_owned()
            }
        };
        let hashed = name.starts_with("app-") && (name.ends_with(".js") || name.ends_with(".css"));
        let _ = writeln!(
            table,
            "    Asset {{ path: {:?}, content_type: {:?}, identity: include_bytes!({:?}), brotli: {}, gzip: {}, immutable: {hashed} }},",
            format!("/{name}"),
            content_type(name),
            base.display().to_string(),
            variant("br"),
            variant("gz"),
        );
    }
    table.push_str("];\n");
    let _ = std::fs::write(out_root.join("web_table.rs"), table);
}

/// A real build ships `app-<hash>.js`, so the empty `app.js` stand-in is not needed.
fn has_hashed_bundle(names: &[String], fallback: &str) -> bool {
    let ext = fallback.rsplit('.').next().unwrap_or("");
    fallback != "index.html"
        && names
            .iter()
            .any(|n| n.starts_with("app-") && n.ends_with(&format!(".{ext}")))
}

fn copy_with_variants(dir: &Path, out: &Path, name: &str) {
    for suffix in ["", ".br", ".gz"] {
        let file = format!("{name}{suffix}");
        if let Ok(bytes) = std::fs::read(dir.join(&file)) {
            let _ = std::fs::write(out.join(&file), bytes);
        }
    }
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}
