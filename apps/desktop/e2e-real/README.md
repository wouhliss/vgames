# Real-application E2E (INS-09)

The **release** launcher, driven through WebDriver, against the **real** API. Nothing is
mocked: the API runs on Postgres with fs storage, packages are published with the `vgames`
CLI, and the launcher talks to it over HTTPS like it would in production.

```
specs/*.test.ts (node --test) + lib/webdriver.ts (W3C WebDriver over fetch)
  └─ tauri-driver ─ WebKitWebDriver ─ target/release/vgames-desktop   (one per instance)
                                         │ https://localhost:8443
                                    stunnel (fixtures/tls.sh, throwaway CA)
                                         │ http://127.0.0.1:18080
                                    vgames-api (debug build, fake Discord, fs storage)
```

## Running it

Linux only (WebKitWebDriver). From the repository root:

```sh
sudo apt-get install -y webkit2gtk-driver stunnel4 xvfb dbus jq
cargo install tauri-driver --locked
pnpm install && pnpm --filter @vgames/desktop build
cargo build --release -p vgames-desktop --features tauri/custom-protocol   # embeds the UI, as `tauri build` does
cargo build -p vgames-api -p vgames-cli
E2E_DATABASE_URL=postgres://vgames:vgames-dev-only@localhost:5432/vgames_e2e \
  apps/desktop/e2e-real/run.sh 1,2      # 1 = M1 only
```

The database must be a throwaway one: it is migrated and written to. `run.sh` starts
everything under Xvfb when `DISPLAY` is unset, and stops everything on exit, including
removing the CA from the system trust store.

No WebdriverIO: its dependency tree includes `jszip` ("MIT OR GPL-3.0-or-later"), which the
repository's dependency review refuses, and the suite needs a dozen commands. `lib/webdriver.ts`
speaks the W3C protocol tauri-driver serves directly, with no dependency; selectors are XPath
built from roles and names (`button("Continue")`, `field("Server address")`, `navLink("Browse")`).

## Fixtures

| File | What it does |
|---|---|
| `fixtures/api.sh up <dir> <db> <api-port> <https-port>` | Generates the root and publisher keys (`scripts/e2e/key-pipeline.sh init`, test passphrases), migrates and starts a debug `vgames-api` with fake Discord, the bootstrap owner `100000000000000001` and fs storage under `<dir>/objects`. |
| `fixtures/tls.sh up <dir> <https-port> <api-port>` | A throwaway CA (2 days, key deleted after signing), a `localhost` certificate, the CA added with `update-ca-certificates` (needs sudo), and stunnel on `https://localhost:<https-port>`. `down` removes the CA again. |
| `fixtures/publish.sh <dir> <server>` | What an owner does with the CLI: `vgames login` (fake Discord), trust bundle v1 with a publisher key, the package `e2e-game` created, a signed `linux-x86_64` release published, and the package set to `published`. Its `bin/game` appends to `$HOME/.vgames-e2e-runs` and then sleeps, so it can be seen running and stopped. |
| `fixtures/instance.sh <dir> <port>` | One launcher instance: its own `HOME`, `XDG_*_HOME`, `XDG_RUNTIME_DIR` and D-Bus session (`dbus-run-session`), `fixtures/xdg-open` as its browser, and `tauri-driver` on `<port>` (WebKitWebDriver on `<port>+1`). |
| `fixtures/xdg-open` | The instance's browser: for the sign-in URL it completes the fake Discord page (`scripts/e2e/fake-discord-browser.sh`) and writes the `vgames://` link to `$HOME/signin-link`, which the suite pastes into "Sign-in code". |

Why not relax anything instead: release builds refuse plain `http` even on loopback
(`servers::discovery::normalize_url`) and ignore `VGAMES_PROFILE` (`paths.rs`). Neither may
gain a test flag, so the API is served over HTTPS and instances are isolated by environment.
On Linux the single-instance lock is the session-bus name `app.vgames.launcher.SingleInstance`
and the keychain is the session's Secret Service: two instances on one bus would merge (the
second hands its arguments over and exits 0). Each instance's session has no Secret Service,
so the launcher uses its file vault there.

**One step does not go through the UI:** "Choose folder" opens the native folder picker,
which WebDriver cannot drive. The suite calls `library_add`, the command the picker's result
goes to, through `window.__TAURI_INTERNALS__.invoke`, then reloads. Everything else is
clicks and typing on what a player sees.

## The scenarios

`specs/launcher.test.ts`, in order, on one instance:

- **Part 1 (M1):** add `https://localhost:8443` → the fingerprint shown equals the one the
  server publishes → "It matches, continue" → "Sign in with Discord" → paste the link → the
  library folder → "Your library is empty" and "This server has no packages yet".
- **Part 2 (M2):** `fixtures/publish.sh` → Browse → the package page → Install into the
  default library → Downloads shows the job, then "Installed" → Play (the game runs) → Stop
  → Verify files ("Repaired version …") → Uninstall → the library is empty again.

## Adding scenarios (PLAY, GAME)

- Add a `test(...)` to `specs/launcher.test.ts` when it continues the same player's story
  (it runs after part 2 when `E2E_PARTS` includes your part), or a new `specs/<name>.test.ts`
  with its own `before` that connects to the instance (`Session.create(port, app)`).
- **Two players (GAME-12, M3 invite):** start a second instance in `run.sh` with
  `fixtures/instance.sh "$work/b" 4454` (another port pair) and sign it in as another Discord
  account: set `DISCORD_ID`/`DISCORD_NAME` in that instance's environment, which
  `scripts/e2e/fake-discord-browser.sh` reads. Assert two live launcher PIDs before driving
  them.
- Prefer the role and name helpers (`button`, `field`, `heading`, `menuItem`, `navLink`) with
  strings taken from `src/i18n/en.ts`; the UI has few `data-testid`s, and these tests should read
  like what a player does. `click` and `type` wait until the element is clickable and retry
  transient refusals (an entrance animation), so do not add sleeps.
- Screenshots and logs as CI artifacts only, and with fake data only.

## CI

Nightly in `.github/workflows/e2e.yml` (job `launcher`, parts 1 and 2). Part 1 is not on
PRs: the release build alone takes longer than the 10 minutes INS-09 allows for a PR job.
