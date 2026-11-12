---
id: web-form
blocked_by: []
---

# Web submit form with collapsed Additional Parameters

Wire `options` through `POST /api/jobs` (reusing handler validation) and add a collapsed "Additional Parameters" `ExpandableSection` with the five fields to the Submit modal. Pure `buildOptions` in `web/src/options.ts`, tested with `bun test`; `api.submit` carries `options` only when given; an HTTP test covers accepted and rejected options.
