//! The embedded web bundle. `build.rs` generates the asset table from `web/dist`
//! (or the vendored copy), including the brotli and gzip variants that `web/build.ts`
//! precompressed, so serving never compresses at runtime and adds no dependency.

pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub identity: &'static [u8],
    pub brotli: Option<&'static [u8]>,
    pub gzip: Option<&'static [u8]>,
    /// Content-hashed file name: the bytes behind this path never change.
    pub immutable: bool,
}

include!(concat!(env!("OUT_DIR"), "/web_table.rs"));

pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";
pub const REVALIDATE: &str = "no-cache";

pub struct Served {
    pub content_type: &'static str,
    pub body: &'static [u8],
    pub encoding: Option<&'static str>,
    pub cache_control: &'static str,
}

pub fn lookup(path: &str, accept_encoding: &str) -> Option<Served> {
    find(ASSETS, path).map(|asset| serve(asset, accept_encoding))
}

fn find<'a>(assets: &'a [Asset], path: &str) -> Option<&'a Asset> {
    let path = if path == "/" || path.starts_with("/jobs/") {
        "/index.html"
    } else {
        path
    };
    assets.iter().find(|a| a.path == path)
}

fn serve(asset: &Asset, accept_encoding: &str) -> Served {
    let (encoding, body) = match pick_encoding(
        accept_encoding,
        asset.brotli.is_some(),
        asset.gzip.is_some(),
    ) {
        Some("br") => (Some("br"), asset.brotli.unwrap_or(asset.identity)),
        Some("gzip") => (Some("gzip"), asset.gzip.unwrap_or(asset.identity)),
        _ => (None, asset.identity),
    };
    Served {
        content_type: asset.content_type,
        body,
        encoding,
        cache_control: if asset.immutable {
            IMMUTABLE
        } else {
            REVALIDATE
        },
    }
}

/// Picks `br` over `gzip` over identity among what the client accepts (q > 0) and
/// what exists.
pub fn pick_encoding(accept: &str, has_br: bool, has_gzip: bool) -> Option<&'static str> {
    let quality = |name: &str| -> f32 {
        let mut star = None;
        for part in accept.split(',') {
            let mut pieces = part.split(';');
            let token = pieces.next().unwrap_or("").trim().to_ascii_lowercase();
            let q = pieces
                .filter_map(|p| p.trim().strip_prefix("q="))
                .find_map(|v| v.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            if token == name {
                return q;
            }
            if token == "*" {
                star = Some(q);
            }
        }
        star.unwrap_or(0.0)
    };
    if has_br && quality("br") > 0.0 {
        Some("br")
    } else if has_gzip && quality("gzip") > 0.0 {
        Some("gzip")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &[Asset] = &[
        Asset {
            path: "/index.html",
            content_type: "text/html; charset=utf-8",
            identity: b"index",
            brotli: None,
            gzip: None,
            immutable: false,
        },
        Asset {
            path: "/app-1a2b3c.js",
            content_type: "text/javascript; charset=utf-8",
            identity: b"plain",
            brotli: Some(b"br"),
            gzip: Some(b"gz"),
            immutable: true,
        },
    ];

    /// A missing asset serves an empty marker response, which every assertion rejects.
    fn served(path: &str, accept: &str) -> Served {
        match find(TABLE, path) {
            Some(asset) => serve(asset, accept),
            None => Served {
                content_type: "missing",
                body: b"",
                encoding: None,
                cache_control: "missing",
            },
        }
    }

    fn script(accept: &str) -> Served {
        served("/app-1a2b3c.js", accept)
    }

    #[test]
    fn brotli_wins_then_gzip_then_identity() {
        assert_eq!(script("gzip, deflate, br").encoding, Some("br"));
        assert_eq!(script("gzip, deflate, br").body, b"br");
        assert_eq!(script("gzip").encoding, Some("gzip"));
        assert_eq!(script("gzip").body, b"gz");
        assert_eq!(script("deflate").encoding, None);
        assert_eq!(script("").body, b"plain");
    }

    #[test]
    fn quality_zero_refuses_an_encoding_and_star_accepts_any() {
        assert_eq!(script("br;q=0, gzip;q=0.5").encoding, Some("gzip"));
        assert_eq!(script("br;q=0, gzip;q=0").encoding, None);
        assert_eq!(script("*").encoding, Some("br"));
        assert_eq!(script("*;q=0").encoding, None);
        assert_eq!(script("GZIP").encoding, Some("gzip"));
    }

    #[test]
    fn missing_variants_fall_back_to_identity() {
        let index = served("/index.html", "br, gzip");
        assert_eq!(index.encoding, None);
        assert_eq!(index.body, b"index");
    }

    #[test]
    fn only_hashed_assets_are_immutable_and_the_shell_revalidates() {
        assert_eq!(script("br").cache_control, IMMUTABLE);
        let index = served("/", "");
        assert_eq!(index.cache_control, "no-cache");
        assert_eq!(
            find(TABLE, "/jobs/01J9ABCDEF").map(|a| a.path),
            Some("/index.html")
        );
        assert!(find(TABLE, "/nope").is_none());
    }
}
