# Launcher accessibility and performance checklist (A3-T12)

What is automated, and what a person checks before a release. Every screen must work with a mouse, the
keyboard alone, and a controller alone (D-pad / left stick moves, A activates, B goes back, LB/RB switch
tabs). In the mock (`pnpm --filter @vgames/desktop dev:mock`) the keyboard stands in for the controller:
the same intents arrive as `ui-nav` events, which the e2e suite also sends.

## Automated

| Check | Where | Bar |
|---|---|---|
| axe on every screen in dark, light and high-contrast | `e2e/a11y.spec.ts` | zero violations at "serious" or above |
| axe on dialogs, errors and states | each feature's `e2e/*.spec.ts` | same |
| Keyboard-only and controller-only flows | `e2e/{onboarding,library,browse,downloads,friends,update,saves,controllers,publish}.spec.ts` | complete without a mouse |
| Roles, focus trap, Escape/B, focus return | `src/components/*.test.tsx` | |
| No heap, DOM or listener growth when navigating | `e2e/memory.spec.ts` (main routes, and settings sections + Publish) | < 256 KB per 100 cycles, flat nodes and listeners |
| 5,000 installed games scroll without dropped frames | `e2e/library.perf.spec.ts` | p95 ≤ 17 ms, none > 34 ms |
| Initial JS and route chunk sizes | `pnpm --filter @vgames/desktop size` (CI "typescript" job) | initial ≤ 250 KB gzipped, each route chunk ≤ 100 KB |

## By hand, per release (keyboard only, then controller only)

Start from a fresh profile (`?mock=fresh`) and from a signed-in one (`?mock=ready`; `?mock=admin` for Publish).
Tick a row only when every step works and the focus ring is always visible.

1. **Onboarding.** Address → fingerprint check → sign in (Open browser again, Paste code) → library folder → done. Each error message is read out when it appears.
2. **Shell.** Skip link reaches the content. Tab and arrows reach the sidebar, server switcher, account menu and the download indicator. Opening a menu focuses its first item; Escape or B closes it and returns focus. After a page change, focus is on the page title.
3. **Library.** Arrow through the grid and the list; Play, the tile menu (every action), Favorite, Add to collection, Collections dialog (create, rename, reorder, delete); LB/RB switch collections.
4. **Browse and details.** Search, genre filter, open a game, screenshot viewer (arrows, B returns to the thumbnail), Install dialog (library picker, not-enough-space link), compatibility blockers.
5. **Downloads.** Pause, resume, cancel dialog, reorder (Alt+Up/Down or the menu), every error's action.
6. **Friends.** Tabs with LB/RB, add friend by code, requests, a conversation (composer, Try again), safety number, invite card → Accept → install dialog.
7. **Settings.** The section list, then each section: Storage (add/remove/move), Compatibility (overrides), Cloud saves (conflict dialog: nothing preselected; history; Restore), Controllers (tester, mapping editor), Overlay (record a shortcut; Escape cancels), Updates (What's new scrolls with the D-pad).
8. **Update banner.** Reaches What's new; Install and restart is disabled with a reason while a game runs.
9. **Publish (admins).** Game → folder (invalid files list scrolls) → version → key → upload → Publish confirmation → Withdraw.
10. **Trust problem screen.** Cannot be dismissed; nothing behind it is reachable.

Also check, once per release: 200 % text size and a 960 px wide window (no clipped focus rings, no
horizontal scrolling), `prefers-reduced-motion` (no movement), and a screen reader announcing toasts once.
