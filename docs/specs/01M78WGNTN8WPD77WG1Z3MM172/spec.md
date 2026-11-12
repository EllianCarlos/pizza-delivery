# Top header (top menu) for the pizza-delivery web UI

## Problem

The web UI (`web/src/App.tsx`) is a Cloudscape `AppLayout` with only a collapsible `SideNavigation`. There is no persistent top bar: on a phone the side nav is hidden behind a hamburger, so moving between Jobs, Inbox and Events takes extra taps, and nothing on screen identifies the app. The "Enable notifications" button sits loose at the top of the page body, above every page's content, which is the wrong place for an app-wide setting. The user asked for "a top header, like a top menu", kept small ("just a few things for now"), and for it to be deployed to the running daemon (`pizza-delivery-daemon.service`, serving https://pizza.elliancarlos.fyi) once merged.

## Solution

Web only; the TUI is unchanged. No new dependency: Cloudscape `TopNavigation` ships in `@cloudscape-design/components` ^3, already a dependency.

1. **Pure model helper** in `web/src/model.ts`:
   - `TOP_NAV_ROUTES = ["jobs", "inbox", "events"] as const` (a subset of `AGENT_WORKER_ROUTES`; Artifacts stays side-nav only to keep the bar small).
   - `topNavItems(opts: { canEnablePush: boolean }): TopNavItem[]`, where
     `TopNavItem = { kind: "link"; text: string; href: string } | { kind: "enable-push"; text: "Enable notifications" }`.
     It returns one `link` per `TOP_NAV_ROUTES` entry, in that order, with `text` "Jobs" / "Inbox" / "Events" and `href = hashFor(route)`, followed by the `enable-push` item only when `canEnablePush` is true.
   - No React or DOM imports, so `bun test` covers it like the rest of `model.ts`.
2. **Render the header** in `web/src/App.tsx`:
   - Import `TopNavigation` from `@cloudscape-design/components/top-navigation`.
   - Wrap the return in a fragment: `<div id="top-nav" style={{ position: "sticky", top: 0, zIndex: 1002 }}><TopNavigation … /></div>` followed by `<AppLayout headerSelector="#top-nav" …>` so AppLayout offsets its sticky elements by the header height.
   - `identity = { href: hashFor("home"), title: "Pizza delivery", logo: { src: "/icon.svg", alt: "Pizza delivery" } }`.
   - `utilities` = `topNavItems({ canEnablePush: pushSupported() && !pushOn })` mapped to Cloudscape items: `link` → `{ type: "button", text, href }`; `enable-push` → `{ type: "button", text, iconName: "notification", onClick }`, where `onClick` runs the existing `enablePush(api)` / `setPushOn(true)` / `setPushError` logic unchanged.
   - `i18nStrings = { overflowMenuTriggerText: "More", overflowMenuTitleText: "All" }` so narrow screens collapse utilities into an overflow menu.
   - Remove the standalone "Enable notifications" `<Button>` from the content area. The `pushError` alert stays in the content area.
   - `SideNavigation`, routing (`routeFromHash`/`hashFor`), and every page stay as they are.
3. **Ship**: open a non-draft PR against `main`; the pipeline reviews and auto-merges.
4. **Deploy** after the merge commit is on `origin/main`: run `scripts/deploy.sh` (syncs the deploy checkout to `origin/main`, builds the web bundle and release binary, restarts the daemon) inside a detached transient unit, `systemd-run --user --unit=pizza-deploy-<unix-ts> --collect <path>/scripts/deploy.sh`, because the job's own agent runs inside the daemon's cgroup and a direct restart would kill the deploy mid-way. Prefer a moment with no other running stages, but do not block indefinitely. Never `systemctl stop` the daemon (it is a transient `systemd-run` unit and would be collected); `deploy.sh` uses `restart`.

Out of scope: TUI changes, a GitHub link, a "Submit job" shortcut in the header, dark-mode toggles, user menus, search.

## Acceptance criteria

- `topNavItems({ canEnablePush: false })` returns exactly three `link` items, Jobs, Inbox, Events in that order, with hrefs `#/agent-workers/jobs`, `#/agent-workers/inbox`, `#/agent-workers/events`.
- `topNavItems({ canEnablePush: true })` returns the same three links followed by one `enable-push` item with text "Enable notifications".
- Every `link` href round-trips: `routeFromHash(item.href)` equals the route it was built from.
- The web UI shows a top bar on every route with the pizza logo and "Pizza delivery" title; clicking the title goes to `#/` (Home).
- The bar shows Jobs, Inbox and Events; clicking each changes the hash and the page, and the SideNavigation still works and highlights the active route.
- When push is supported and not yet enabled, "Enable notifications" appears in the bar (not in the page body) and enabling it hides the item; a failure still shows the error alert in the content area. When push is unsupported the item is absent.
- The bar stays at the top while scrolling and does not overlap AppLayout's sticky content (table headers, split panel, nav drawer) at desktop and phone (≤ 600 px) widths; at phone width the utilities collapse into the overflow menu.
- `cd web && bun install --frozen-lockfile && bun run typecheck && bun test && bun run build` passes, and `cargo build -p pizza-delivery` passes from the repo root (the binary embeds the web bundle).
- After the PR merges, the deploy unit's journal (`journalctl --user -u pizza-deploy-<ts>`) shows `deploy.sh` finishing, `pizza-delivery-daemon.service` is active with the new binary, and https://pizza.elliancarlos.fyi serves a bundle containing the top navigation.

## Seams

- **`topNavItems` in `web/src/model.ts`** (primary test seam): pure function from `{ canEnablePush }` to item descriptors. Unit-tested in `web/src/model.test.ts` with `bun test`, covering link order/text/hrefs, the `enable-push` gating, and the `hashFor`/`routeFromHash` round-trip. This is where the header's content rules live, so the React layer only maps descriptors to Cloudscape props.
- **`App.tsx` rendering** (no automated test): the repo has no DOM test library and adding one is out of scope; covered by `bun run typecheck` (Cloudscape prop types catch wrong `utilities`/`identity` shapes), `bun run build`, and a manual check of the built page at desktop and phone widths.
- **Deploy** (operational seam, no automated test): verified by the transient unit's journal, `systemctl --user status pizza-delivery-daemon.service`, and fetching the live site.
