---
audience: internal
component: launcher
type: added
---
A3-T07 (first part): Settings shell with one URL per section (`/settings/<section>`) and General, Servers, Account, Storage, Downloads, Updates and About. Pending IPC contract in `apps/desktop/src/ipc/contract/settings.ts` (`account_sessions`, `account_session_revoke`, `credential_storage`, `library_set_default`, `library_remove`, `download_settings_get|set`, `social_settings_get|set`, `overlay_packages`, `overlay_package_set`, `app_licenses`). The mock backend can persist settings across a simulated restart (`persistKey`). The fingerprint prefix now meets contrast on cards.
