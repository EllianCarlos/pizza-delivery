---
id: top-nav-ui
blocked_by: [top-nav-model]
---

# Render Cloudscape `TopNavigation` above `AppLayout`

Wire the helper into `web/src/App.tsx` (see spec, Solution step 2), verify, and open the PR.

## Do

- Import `TopNavigation` from `@cloudscape-design/components/top-navigation`.
- Return a fragment: `<div id="top-nav" style={{ position: "sticky", top: 0, zIndex: 1002 }}><TopNavigation …/></div>` then `<AppLayout headerSelector="#top-nav" …>`.
- `identity`: `{ href: hashFor("home"), title: "Pizza delivery", logo: { src: "/icon.svg", alt: "Pizza delivery" } }`.
- `utilities`: map `topNavItems({ canEnablePush: pushSupported() && !pushOn })` — `link` → `{ type: "button", text, href }`; `enable-push` → `{ type: "button", text, iconName: "notification", onClick }` with the existing `enablePush(api)` / `setPushOn(true)` / `setPushError` logic moved in unchanged.
- `i18nStrings`: `{ overflowMenuTriggerText: "More", overflowMenuTitleText: "All" }`.
- Delete the standalone "Enable notifications" `<Button>` from the content; keep the `pushError` alert there. Leave `SideNavigation`, routes and pages untouched.

## Verify

- `cd web && bun install --frozen-lockfile && bun run typecheck && bun test && bun run build`.
- `cargo build -p pizza-delivery` from the repo root.
- If a browser is available, load the built UI at desktop and ≤ 600 px widths: bar is sticky, no overlap with table headers / split panel / nav drawer, utilities collapse to the overflow menu on phone, links navigate and the side nav highlights the active route.

## Ship

Commit, push the branch, open a non-draft PR against `main` (body via the `pr` skill conventions). Let the pipeline's review and auto-merge run.

## Done when

All acceptance criteria in the spec except the deploy ones hold, and the PR is open against `main`.
