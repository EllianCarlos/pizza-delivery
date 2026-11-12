---
id: api-variants
blocked_by: [core-filter-module]
---

# API request and response variants

In `api.rs` add `Request::Inbox`, `Request::Events`, `Request::Artifacts` (filter, `query`, `limit`) and `Response::InboxItems`, `Response::EventItems`, `Response::ArtifactItems`. Job filters carry string refs (resolved by the handler). Fix exhaustive matches in client.rs, handler.rs and server.rs with a placeholder arm that returns a Usage error until the handler ticket lands.

Test first: new variants round-trip as tagged JSON; existing `Status` JSON is unchanged.
