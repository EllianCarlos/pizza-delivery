const INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/index.html"));
const SCRIPT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/app.js"));
const STYLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/app.css"));
const MANIFEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/manifest.webmanifest"));
const WORKER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/sw.js"));
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/icon.svg"));
const ICON_192: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/icon-192.png"));
const ICON_512: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/icon-512.png"));

pub fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX)),
        "/app.js" => Some(("text/javascript; charset=utf-8", SCRIPT)),
        "/app.css" => Some(("text/css; charset=utf-8", STYLE)),
        "/manifest.webmanifest" => Some(("application/manifest+json", MANIFEST)),
        "/sw.js" => Some(("text/javascript; charset=utf-8", WORKER)),
        "/icon.svg" => Some(("image/svg+xml", ICON)),
        "/icon-192.png" => Some(("image/png", ICON_192)),
        "/icon-512.png" => Some(("image/png", ICON_512)),
        _ if path.starts_with("/jobs/") => Some(("text/html; charset=utf-8", INDEX)),
        _ => None,
    }
}
