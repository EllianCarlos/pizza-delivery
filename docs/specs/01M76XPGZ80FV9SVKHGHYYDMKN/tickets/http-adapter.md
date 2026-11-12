---
id: http-adapter
blocked_by: []
---

# Loopback HTTP/JSON adapter for submit and dry-run

Add an opt-in `http_listen` config key and an `std::net` HTTP module in `crates/pizza-delivery` serving `POST /api/submit`, `POST /api/dry-run` and `GET /` (placeholder page until the web form lands, via an embedded asset constant). Reuse `handle` and `dry_run`; map usage errors to 400. Enforce loopback bind, Host/Origin checks, JSON content type and 64 KiB body cap. First verify cargo works in the devenv/nix shell and `cargo test` is green at baseline. Tests: pure routing unit tests, config tests, `tests/http.rs` end to end.
