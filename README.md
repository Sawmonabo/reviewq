# ReviewQ

ReviewQ is a local workspace for reviewing GitLab merge requests alongside their Jira tickets and native Codex or Claude conversations. Start in a terminal, read the selected file and its discussions in the browser, and send feedback to the same review run. The browser, CLI and Model Context Protocol (MCP) clients share one Rust service backed by SQLite. Native hosts retain their transcripts.

## Build and install

On native Linux or WSL with a running systemd user manager, build from a clean checkout. Use GNU Make, Rust 1.100.0, Node.js 24.19.0 or newer, pnpm 12.15.0 and `readelf` from GNU binutils. Build with a Linux runtime compatible with the target machine.

```sh
make web-install
make package
```

The package target prints its directory, manifest digest and required platform loader. It validates the Claude plugin and marketplace with `~/.local/bin/claude` when present. Set `REVIEWQ_PACKAGE_CLAUDE` to select another executable. Use the printed package directory in place of `/absolute/path/to/bundle`.

```sh
/absolute/path/to/bundle/bin/reviewq install --from /absolute/path/to/bundle
~/.local/bin/reviewq
```

The first command installs and starts the user service. The second opens the dashboard. Use `reviewq status` to check the running profile, `reviewq stop` and `reviewq start` to control it, and `reviewq logs` for the service journal. The CLI lives in `~/.local/bin`. Install a new verified bundle with the same command to update. `reviewq uninstall` removes the installed software and retains the local database and runtime settings.

The installer prints the runtime configuration path and reports missing native providers by name. Configure exact native executables there with `codexCommand` and `claudeCommand` if they live outside `~/.local/bin`. In an open Claude Code session, use `/reload-plugins` after installation or an update. Run `reviewq notify-test` to check the local desktop notification service. [Runtime configuration and recovery](docs/specs/operations.md)

## Develop

For UI development against your installed ReviewQ service, run `pnpm --dir web run dev --installed`. Open the printed loopback URL in the browser profile signed in through `reviewq`. Vite updates the UI live and forwards read-only requests to the running service. The installed backend updates when you install a new package.

Install the locked browser dependencies once. The development URL serves the current checkout through Vite. Saving browser code or applying a branch change there uses hot module replacement when possible. The installed ReviewQ URL serves its packaged release until the next installation.

```sh
make web-install
pnpm --dir web run dev --profile "$PWD/.agents/tmp/reviewq-dev-profile"
```

The runner prints its loopback frontend URL and uses port 5173 by default. Set `REVIEWQ_WEB_PORT=0` to bind an available port. Run without `--profile` for browser-only development. With an explicit separate profile, the same URL also proxies the Rust API. Save Rust sources or merge a branch that changes them to rebuild the checkout's backend. The current backend keeps serving during the build and stays available if compilation fails. After a successful build the runner restarts its backend against the same profile and browser port. Press Enter in an interactive terminal for a fresh sign-in link. Ctrl+C stops the development processes.

Use `--backend-port 8790 --native-port 8791` to select fixed backend ports and reuse the browser session across runner launches. The backend selects available loopback ports by default. The native port can change on restart unless explicitly fixed. Add `--read-only` to limit the frontend to reads, normal sign-in and report quote lookup. Add `--validation` for a disposable runtime with background provider polling and native actions disabled. Use a separate profile for development, with its own SQLite state. Stop an existing daemon for that profile before starting the runner.

Run checks from a second terminal. Use cargo-nextest, rustfmt and Clippy. The browser suite uses the Chromium build that matches the locked Playwright version.

```sh
make check
make web-build
make web-test
```

`make check` runs crate boundary checks, Rust formatting/lints/tests/doctests and web static checks. The next two commands build production assets and run Playwright. On a fresh development machine, `pnpm --dir web exec playwright install chromium` provisions the browser. `make check-low` uses one Rust build job and one test slot. Developers using mbx can run `make check-cached`. [Make targets](Makefile) · [Rust toolchain](rust-toolchain.toml) · [Web tooling](web/package.json)

## Find the design or the next task

| Question | Start here |
| --- | --- |
| What owns the review, its data and the user interaction? | [Architecture](docs/architecture.md) and [source layout](docs/project-layout.md) |
| Which behaviors must survive implementation? | [Data contracts](docs/specs/data-contracts.md), [persistence](docs/specs/persistence.md), [native sessions](docs/specs/agent-sessions.md), [queue and settings](docs/specs/queue-and-settings.md), [review workflow](docs/specs/review-workflow.md), [browser experience](docs/specs/browser-experience.md) and [operations](docs/specs/operations.md) |
| How do I choose a package or place code? | [Dependency decisions](docs/dependency-decisions.md), [Rust conventions](docs/rust-conventions.md), [React and TypeScript conventions](docs/react-typescript-conventions.md) and [repository rules](AGENTS.md) |
| Which work can run independently? | The [implementation roadmap](docs/plans/roadmap.md) lists prerequisite revisions, file owners and a brief for each task. The integration owner wires shared files. A separate reviewer exercises the resulting revision before dependent work advances. |
| What qualifies the product for release? | [Acceptance criteria](docs/specs/acceptance.md), [browser qualification](docs/plans/06-release/browser-qualification.md) and the [installed release check](docs/plans/06-release/release-check.md) define the fixture, real-host and desktop evidence required from the built artifact. |
