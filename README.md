# ReviewQ

ReviewQ is a local workspace for reviewing GitLab merge requests alongside their Jira tickets and native Codex or Claude conversations. Start in a terminal, read the selected file and its discussions in the browser, and send feedback to the same review run. The browser, CLI and Model Context Protocol (MCP) clients share one Rust service backed by SQLite. Native hosts retain their transcripts.

## Build and install

On native Linux or WSL with a running systemd user manager, build from a clean checkout. Use GNU Make, Rust 1.100.0, Node.js 24.19.0 or newer, and pnpm 12.15.0.

```sh
make web-install
make package
```

The package target prints its directory and manifest digest. Use that directory in place of `/absolute/path/to/bundle`.

```sh
/absolute/path/to/bundle/bin/reviewq install --from /absolute/path/to/bundle
~/.local/bin/reviewq
```

The first command installs and starts the user service. The second opens the dashboard. Use `reviewq status` to check the running profile, `reviewq stop` and `reviewq start` to control it, and `reviewq logs` for the service journal. The CLI lives in `~/.local/bin`. Install a new verified bundle with the same command to update. `reviewq uninstall` removes the installed software and retains the local database and runtime settings.

The installer prints the runtime configuration path and reports missing native providers by name. Configure exact native executables there with `codexCommand` and `claudeCommand` if they live outside `~/.local/bin`. In an open Claude Code session, use `/reload-plugins` after installation or an update. [Runtime configuration and recovery](docs/specs/operations.md)

## Develop

Use cargo-nextest, rustfmt and Clippy for checks. The browser suite uses the Chromium build that matches the locked Playwright version. Install the locked web dependencies and run the web development server from the repository root.

```sh
make web-install
cargo run --locked -p reviewq -- --version
pnpm --dir web run dev
```

The dev server prints its loopback URL and uses port 5173 by default. Set `REVIEWQ_WEB_PORT=0` to select an available port. Run the checks from a second terminal.

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
