---
id: core-filter-module
blocked_by: []
---

# Core filter and search module

Add `crates/pizza-delivery-core/src/filter.rs` (export from lib.rs): `EventFilter`, `ArtifactFilter`, `InboxFilter` structs, projection types `InboxItem` and `ArtifactItem` (serde), and pure functions that take `&[Event]`, a filter, an optional query and optional limit and return newest-first results. AND across properties, OR within repeated values, case-insensitive substring search ANDed with the filter, inclusive time bounds, `min_confidence` excludes entries without confidence. Job refs are already-resolved `JobId`s here.

Test first (unit tests in the module): each property alone, AND, OR, search, ordering, limit, empty filter, empty query.

Add Filter, Search and Inbox item to GLOSSARY.md.
