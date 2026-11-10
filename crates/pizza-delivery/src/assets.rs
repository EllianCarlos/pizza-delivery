const INDEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/index.html"));
const SCRIPT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/app.js"));
const STYLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/app.css"));

pub fn lookup(path: &str) -> Option<(&'static str, &'static [u8])> {
    match path {
        "/" | "/index.html" => Some(("text/html; charset=utf-8", INDEX)),
        "/app.js" => Some(("text/javascript; charset=utf-8", SCRIPT)),
        "/app.css" => Some(("text/css; charset=utf-8", STYLE)),
        _ => None,
    }
}
