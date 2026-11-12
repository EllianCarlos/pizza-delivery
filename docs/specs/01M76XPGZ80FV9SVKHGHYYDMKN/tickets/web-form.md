---
id: web-form
blocked_by: [http-adapter]
---

# Web submit form with collapsed Additional Parameters

Create `web/` with `index.html`, a JS module exporting `buildSubmitBody(fields)` and the page wiring (expander collapsed by default, five option fields in order, Preview and Submit buttons, inline server errors). No npm dependencies. Test `buildSubmitBody` with `bun test` (omit `options` when collapsed/empty, types, trimming, risk splitting). Embed the assets in the daemon so `GET /` serves the real form; add a test that `GET /` contains the expander.
