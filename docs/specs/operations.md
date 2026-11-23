# Installation, updates and recovery

ReviewQ runs as a personal Linux or WSL2 application. One Rust executable serves the production browser and accepts terminal commands. The installed directory includes version-matched web files and native host assets. An installed user service continues polling after the terminal closes. A foreground validation process can run alongside it in a separate private directory.

## What the operator runs

| Command | Observable result |
|---|---|
| Bundled `bin/reviewq install --from /absolute/release` | Verify the local bundle, install or update the user's managed launcher and service, provision the native assets and print the active build and dashboard address. A fresh install opens first-run setup on first visit. An ordinary upgrade reuses the current supported data. |
| `reviewq` or `reviewq open` | Start or reuse the service, wait for the exact installed profile to become ready and open its dashboard. Print the local dashboard URL if browser handoff fails or with `--print-url`. Repeated opens reach one service. |
| `reviewq start`, `reviewq stop` | Start/resume or stop local monitoring. A repeated request is safe. Report the resulting service state. Existing Claude and Codex sessions continue under their native hosts. |
| `reviewq status`, `reviewq refresh` | Status reports running state, last successful synchronization, current failure/refresh and queue counts. Refresh waits for the requested/coalesced refresh result. A failure or bounded wait expiry reports which result remains pending. These commands use the same profile and application state as the browser. |
| `reviewq logs`, `reviewq notify-test` | Show recent sanitized service/UI failures, or send one labeled local test notification and report the OS handoff result. A missing desktop facility is visible. WSL uses the user's owned ReviewQ Windows sender; Linux uses the desktop notification service. |
| `reviewq serve [--port N]` | Run in the foreground using installed paths, exit cleanly on termination, and refuse a second writer for the same profile. The service unit invokes this entry. |
| `reviewq serve --validation --runtime-root /absolute/private/path [--port 8768]` | Serve the production API and web build against private disposable data. Disable provider polling, launches, sends, corrections, follow-up dispatch, notification delivery and personal host/service installation. Use seeded data for inspection. Real native and provider proof uses a separately installed disposable account. |
| `reviewq uninstall` | Disable/stop the owned service and remove owned application and host registrations. Print retained data paths and any user-edited entries requiring attention. A later install can reopen the retained store. |

Run the installed binary from any directory. Loading the browser, using the MCP tools in an ordinary chat and starting a review session are independent actions. The native skill, registration, report, publication, acknowledgement and management command behavior belongs to [native sessions](agent-sessions.md) and [ENTRY](../plans/02-sessions/entrypoints.md).

## Installation and local identity

Use an active systemd user manager for the background installation, a compatible browser and configured native host executables for the integrations selected for use. Verify which OS, architecture, loader, host versions and desktop/WSL interop were exercised with each released artifact. Rust, Node, pnpm and `readelf` are build tools. The packaged Rust server and Claude's host-loaded JSX use the installed binary and provider CLIs at runtime. Building the web UI stays separate from normal Rust compilation. [systemd services](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html) · [Production bundle](https://vite.dev/guide/build.html)

| Personal location | Content and ownership |
|---|---|
| `~/.local/bin/reviewq` | Stable launcher pointing into the active release. Its target is absolute and can be invoked by native hooks independent of shell PATH. |
| `$XDG_DATA_HOME/reviewq/{current,releases,assets}` | Private installation root with atomic active-release link, immutable release directories by bundle ID and retained fingerprinted static assets for already-open tabs. |
| `$XDG_DATA_HOME/reviewq/reviewq.sqlite3` | Current STORE database. SQLite application ID `0x52565132`, current format ID 1. Keeps workflow state, versioned settings, private provider credentials and receipts. The service opens this path after verifying its supported format. Retain it across uninstall. |
| `$XDG_CONFIG_HOME/systemd/user/reviewq.service` | Managed user unit. Starting an installed service opens no browser. Stop/disable via the systemd user manager when removing it. |
| `$XDG_CONFIG_HOME/reviewq/` | Private runtime options and native bridge descriptor. Installed callbacks read `bridge.json` with the installation ID, canonical native address, browser URL and scoped secret. Generate it under owner-only permissions. A terminal skill, command line, log, browser asset or output receipt never contains this credential. |
| `$XDG_STATE_HOME/reviewq/` | Private operational state, retained backups, install journal, stable install/profile lock inodes and sanitized local diagnostics. A lock stays outside any path removed by an update or uninstall. |
| Native host state | The explicit Codex registration skill links under the selected Codex user directory. The Claude marketplace/plugin installs at user scope using the host CLI. Configured checkouts/worktrees receive the separate management skill and exact private MCP grant through ENTRY. Provider sign-in, native histories, preferences and unrelated plugins remain with the native host. |

Resolve unset XDG config, data and state homes to `~/.config`, `~/.local/share` and `~/.local/state`. Honor absolute overrides. Use private owner directories (0700) and files (0600) for state and credentials. Shared parent directories stay under their current owner/mode. Open private files with checked ownership, link type and exclusive creation where appropriate. Installed and validation data/credential paths are disjoint including symlink, hard-link and SQLite sidecar aliases. [XDG base directories](https://specifications.freedesktop.org/basedir/latest/)

Use `http://127.0.0.1:8766` by default for the browser. Any local browser profile can open the running service's dashboard or a saved review link directly. The browser obtains its write proof automatically when needed. When IPv6 loopback is available, bind `[::1]` on the next port for native Linux clients in WSL and record the actual endpoint in the private descriptor. Otherwise use the verified IPv4 loopback for native clients. Validate the configured base port and both bindings before opening writable state. An occupied port yields a conflict, closes any partially acquired listener and preserves its owner. A private validation port pair must be separate from both installed ports. Apply the shared HTTP/MCP authentication and origin rules at the corresponding listener. After verifying the unit/profile owns the bound listener, perform the challenge-authenticated `GET /api/v1/status` and verify its signed response before trusting build digest, installation/profile identity, store support and queue readiness. All listeners are loopback-only. [Browser launch and service readiness](data-contracts.md#browser-launch-and-service-readiness)

## Configure the local runtime

The installed nonsecret file is `${XDG_CONFIG_HOME:-$HOME/.config}/reviewq/runtime.json`. Print this resolved path in the install receipt and in `reviewq status`, including when the user service is stopped. PACK creates it privately on first install, retains it on update/uninstall, and reads it before activating provider hosts. The service reads one validated snapshot at startup.

| Property | Default, validation and use |
|---|---|
| `port` | Integer 1024–65534, default 8766. Bind browser IPv4 loopback and the optional IPv6-native port at base + 1. An explicit foreground `serve --port N` selects that invocation's base. Installed `serve` otherwise uses the file. Validation uses its explicit port, otherwise 8768, and its own private configuration. Never rewrite the saved port from a temporary CLI override. |
| `pollSeconds` | Integer ≥60, default 300. Time between scheduled provider refreshes while connected. A manual refresh still runs through the shared refresh coordinator. Changing the interval does not acknowledge events or change the last successful refresh. Use a checked timer conversion; reject a value beyond the scheduler's representable range. |
| `desktopNotifications` | Boolean, default true. False mutes automatic desktop delivery. Continue recording inbox events, unread counts, deduplication and normal agent/report state. Muting adds no delivery acknowledgement. After re-enabling, apply the normal unread, dismissal, grouping and already-notified rules. Explicit `reviewq notify-test` still sends one labeled diagnostic. |
| `claudeCommand`, `codexCommand` | Nonempty executable names/paths. Defaults resolve from the installed account's home as `~/.local/bin/claude` and `~/.local/bin/codex`. Prefer literal absolute paths in the file. Resolve a bare command using the service PATH, record the effective executable for the operation, and pass arguments separately. A missing executable identifies the unavailable integration; never substitute another provider or account. Provider-native profile selection uses `CLAUDE_CONFIG_DIR` / `CODEX_HOME` in the installed service environment, then the respective host defaults. Do not use a caller's cwd to choose a different profile. |
| `chromiumCommand` | Optional local Chromium executable name or absolute path. ReviewQ uses it with a disposable private browser profile to prepare verified completed Mermaid drawings. Validated drawings stay available until they expire. The reader uses its built-in Mermaid renderer when a matching saved drawing is unavailable. |
| `wslDistribution` | String. A nonempty explicit distribution wins for Windows Terminal handoff. Otherwise use the current WSL distribution from `WSL_DISTRO_NAME`. During installation on WSL, capture the detected distribution as the initial value so a user service without the shell environment can reopen the same distribution. If neither source supplies it, return the exact native copy command. On Linux the option is unused. Pass the selected name as one `wsl.exe --distribution` argument. |

For example, a WSL user whose tools are installed under `/opt/local/bin` can save the following as `runtime.json`. The settings use a three-minute poll and mute desktop pop-ups. The browser will use `127.0.0.1:8910` and the native IPv6 endpoint, when available, will use port 8911.

```json
{
  "port": 8910,
  "pollSeconds": 180,
  "desktopNotifications": false,
  "claudeCommand": "/opt/local/bin/claude",
  "codexCommand": "/opt/local/bin/codex",
  "wslDistribution": "Ubuntu-24.04"
}
```

For a manual edit, save in the same private directory with mode 0600 using an editor with atomic replace, then run `reviewq stop`, `reviewq start` and `reviewq status`. Stop must work through the verified user unit even when the file is invalid. Each start validates the whole object, applies defaults for omitted fields and fails with the path and offending property on invalid JSON, unknown keys, wrong types or impossible port pairs before claiming a writable store. Active settings stay at the previous startup snapshot until restart. A failed restart is visible via status/logs; correct the file and start again. PACK updates it only by exclusive private temp, flush, atomic same-directory rename and parent sync, after comparing the expected original to avoid overwriting simultaneous user edits.

After a port or native profile change, republish the verified bridge locator and reconcile checkout MCP endpoints through ENTRY with the currently committed settings revision. If the port changes, open the new address reported by `reviewq open --print-url`. At the same address, an open tab can keep its selection while recovering from a service restart and can use an ordinary reload after an update. Installed command and host clients discover the verified active listener from its private locator. An unsaved or not-yet-applied port edit cannot redirect a native call. Test a restart with the requested pair occupied and check that the service reports the conflict and the unrelated listener receives no bearer.

Interactive **Settings** saves apply through SQLite. PREFS owns accounts, repository bindings, credentials and the independent Codex recovery preference. FOLLOW owns automatic follow-up policy, per-MR overrides and the shared review concurrency (default 2, range 1–8). Recovery and automatic follow-ups start off. These changes use their own versioned compare-and-set operations and apply without a service restart. The service rejects automation-policy keys in `runtime.json` and points to the matching Settings controls. [Settings](queue-and-settings.md)

## Data choices on install

| Starting point | Result |
|---|---|
| Absent database | Initialize the current schema in a new private file and open setup. New credentials are scoped to this installation. |
| Current-schema store, including a retained profile after uninstall | Reopen it, verify the binary, assets and readiness, and preserve settings, report versions, unread events, pending submissions and native identities. |
| Unrecognized file or user-owned destination | Report the conflicting path before switching the launcher or activating host assets. Preserve the file and its sidecars. |

## Recovery and removal

Hold a kernel-managed advisory lock for competing install, uninstall and release recovery operations. The daemon holds a separate lifetime lock for a specific data profile. Lifecycle requests use the user manager and the install journal to avoid a second writer during the switch. Use unique private sibling candidates, durable intent before each external effect, digest/ownership readback after it and atomic same-directory rename of each published pointer. The journal coordinates multi-file and native-host updates. An install is ready only after the service identifies its build/profile and the recorded host activations have been read back. Subsequent invocations can finish interrupted steps. [Rust file locking and sync](https://doc.rust-lang.org/std/fs/struct.File.html) · [SQLite backup](https://www.sqlite.org/backup.html)

| Interruption or failure | Next invocation |
|---|---|
| While staging or checking a manifest | Existing installation serves the last complete release. Discard or reuse only the current operation's private candidate after rechecking it. Publishing a contradictory candidate requires a new output. |
| After switching a pointer, before host/service readiness | Reacquire the lock, compare recorded expected and observed pointers/files/host versions, complete matching steps and recheck readiness. A third-party edit is reported and retained. Reinstall/repair uses the packaged executable if the launcher has been removed. |
| Same-schema binary rollback | Quiesce the service, verify the retained release's exact store schema and host assets, switch pointers back, restore only owned config changes, restart the previous service according to its recorded enabled/running state, then verify build and data. Preserve committed user work. |
| Uninstall interrupted or owned config edited | Revoke owned access and remove journaled entries whose identity still matches. Preserve modified or unrelated entries and identify their paths. A retry with the launcher or release binary finishes remaining owned cleanup. |

For a same-schema downgrade, invoke the previously retained release's `bin/reviewq install --from /absolute/previous-release`. Verify its manifest, exact database schema and managed host assets before reactivating it. For an interrupted install, retry the exact new bundle and arguments. The journal supplies the last verified step and the previous release/backup paths. Each installed release retains its manifest and complete bundle inputs for these operations.

Uninstall retains private settings, workflow data, backups, operator-authored reports and native histories. Uninstalling the app does not stop a registered host conversation. Qualification covers a working reinstall and a rollback rehearsal in a disposable account. [Packaging](../plans/06-release/packaging.md) · [Installed release checks](../plans/06-release/release-check.md)
