---
audience: internal
component: launcher
type: added
---
A3-T05: Browse (virtualized catalog grid with debounced search, genre filter, sort, paging) and package details (hero, facts, SafeMarkdown description, screenshot viewer, compatibility panel with blockers and Rosetta 2 install, install dialog with library picker). Pending IPC contract in `apps/desktop/src/ipc/contract/catalog.ts` (`catalog_list`, `catalog_genres`, `package_details`, `install_plan`, `install_start`, `rosetta_install`) with mocks. Shared fixes: focus moves to the page title after navigation, tooltips shift to stay inside dialogs, and the router has a first-load fallback.
