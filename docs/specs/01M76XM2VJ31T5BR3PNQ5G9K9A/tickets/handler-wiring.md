---
id: handler-wiring
blocked_by: [api-variants]
---

# Handler wiring

In `handler.rs` implement the three new requests: resolve job refs with the existing resolver (NotFound/Ambiguous errors), reject `since > until` with a Usage error, gather events via `list_jobs` + `read_timeline`, call the core filter functions, return the new Responses.

Test first (handler integration tests with seeded events and fake Env): filter by job ref incl. @last, unknown ref, ambiguous ref, inverted range, AND/OR through the handler, search, limit.
