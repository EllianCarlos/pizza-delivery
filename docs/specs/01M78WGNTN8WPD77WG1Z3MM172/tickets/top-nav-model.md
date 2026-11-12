---
id: top-nav-model
blocked_by: []
---

# Pure `topNavItems` helper in `model.ts`

Add the header's content rules as a pure, tested helper (see spec, Solution step 1).

## Do

- In `web/src/model.ts` add `TOP_NAV_ROUTES = ["jobs", "inbox", "events"] as const`, the `TopNavItem` union type, and `topNavItems(opts: { canEnablePush: boolean }): TopNavItem[]`.
- Links use `hashFor(route)` and the texts "Jobs", "Inbox", "Events"; the `enable-push` item ("Enable notifications") is appended last only when `canEnablePush` is true.
- No React/DOM imports in `model.ts`.
- Write the tests first in `web/src/model.test.ts` (red, then green).

## Tests (`bun test`)

- `canEnablePush: false` → exactly three links, order Jobs, Inbox, Events, hrefs `#/agent-workers/jobs|inbox|events`.
- `canEnablePush: true` → the same three links plus a trailing `enable-push` item.
- Each link's `routeFromHash(href)` equals its route.

## Done when

`cd web && bun run typecheck && bun test` passes.
