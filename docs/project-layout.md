# ReviewQ project layout

The repository contains a Rust workspace, a Vite browser app and their checks. The layout below assigns each planned file to its first production use and its subsequent owner. SLICE implements a narrow working review inside these same paths. The full feature owner takes that implementation forward. Create a module when its first consumer lands. [Task graph and integration ownership](plans/roadmap.md)

## Existing scaffold

| Path | Scope |
| --- | --- |
| `AGENTS.md`, `crates/AGENTS.md`, `web/AGENTS.md` | Repository rules, Rust import policy, browser ownership and interaction requirements |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `rustfmt.toml`, `.cargo/`, `.config/nextest.toml`, `.mbx.toml` | Pinned workspace, per-checkout build configuration, cache and test scheduling |
| `Makefile`, `tools/check-crate-deps.mjs` | Root check commands, workspace package paths and permitted dependency edges |
| `crates/reviewq-*/`, `crates/reviewq/` | Six libraries and the binary package. Each has its own Cargo manifest. The binary owns the existing CLI/version entry. |
| `web/package.json`, `web/pnpm-lock.yaml`, `web/pnpm-workspace.yaml`, `web/tsconfig.json` | One browser dependency graph, frozen install and strict TS build |
| `web/{vite.config,playwright.config,dependency-cruiser,eslint.config}.mjs`, `web/knip.json`, `web/.prettierrc.json` | Dev/build, browser test, import boundaries, lint, unused reachability and formatting |
| `web/index.html`, `web/src/main.tsx`, `web/src/app/App.tsx`, `web/tests/` | Browser entry, application shell and initial browser checks |

The integration writer changes workspace/member manifests, locks, shared module roots, command registries, route assembly and application composition on the current integration revision. Each leaf owner supplies concrete symbols, dependency features, auth/limits, first caller and focused check. Build and review the leaf with its registrations in an existing writer checkout. Merge them together. The downstream owner starts from the accepted revision and regenerates shared outputs from that tree when needed. [Integration procedure](plans/roadmap.md) · [Build tools](../Makefile)

## Rust source

Each package is directly under `crates/` in a directory that matches its package name. Each library has a `src/lib.rs` controlled by the integration writer for public exports. The allowed first-party dependency direction is exact.

| Package | Allowed direct workspace dependencies | Main files and ownership |
| --- | --- | --- |
| `reviewq-domain` | none; standard library only | `src/identity.rs` (CONTRACT). `queue.rs,review_stage.rs` (QUEUE), `settings.rs` (PREFS), `review_action.rs` (ACTION), `report.rs` (REPORT), `followup.rs` (FOLLOW). Pure identity, transitions, classification and invariants. |
| `reviewq-protocol` | none | `src/{common,events}.rs`, `src/bin/export-schema.rs` (CONTRACT). Named DTO leaves `queue,hosts,changes,history` start with CONTRACT/SLICE, then QUEUE/HOST/SOURCES/HISTORY; `notifications,settings,actions,reports,followups,mcp` belong to their feature owner. External Serde/Schemars imports are local to this boundary. |
| `reviewq-app` | `reviewq-domain` | `src/{access,events}.rs` (CONTRACT). `hosts,queue,notifications` start with STORE; `hosts,history,sources,queue` deliver the first slice then the named full feature. `settings.rs` (PREFS). `src/workflow/{actions,reports,report_evidence,followups}.rs` (ACTION, REPORT, FOLLOW). Application-owned inputs/ports define the atomic or provider work each use case needs. |
| `reviewq-store` | `reviewq-app`, `reviewq-domain` | `src/{connection,events}.rs`, `src/schema.sql` and feature definitions in `src/schema/` (STORE). `hosts,queue,notifications` start as the minimal durable adapter, then HOST/QUEUE take ownership. `src/{history,settings,actions,reports,followups}.rs` implement the typed feature ports. All driver code lives here. |
| `reviewq-providers` | `reviewq-app`, `reviewq-domain` | `src/{http,gitlab,jira}.rs`, `src/gitlab/changes.rs` (SOURCES); `src/codex/{connection,session,inputs,capacity}.rs`, `claude/{session,companion}.rs`, `terminal.rs` (HOST); `history/{file,claude,codex,tools}.rs` (HISTORY); `report_file.rs` (REPORT); `desktop.rs` (QUEUE). SLICE introduces the first Codex/GitLab/history subset. |
| `reviewq-transport` | `reviewq-app`, `reviewq-domain`, `reviewq-protocol` | `src/{entry,mcp}.rs` (SLICE → ENTRY), `hosts,history,changes,queue,notifications,settings` for their HTTP adapters; `src/http/{actions,reports,followups}.rs` for workflow handlers. Compose one Axum application, durable and scoped SSE, and the shared MCP endpoint. |
| `reviewq` | `reviewq-app`, `reviewq-domain`, `reviewq-protocol`, `reviewq-store`, `reviewq-providers`, `reviewq-transport` | `src/main.rs` bootstrap/composition (integration writer). The foreground composition root opens STORE with a validated domain path and resolves configured project identities before constructing provider adapters. `src/{cli,checkout_setup,native_client}.rs` (SLICE → ENTRY). `src/cli/action.rs` (ACTION) and `src/cli/report.rs` (REPORT) supply early installed-helper subcommands and their exact native wire requests; the integration writer wires their dispatch. `src/{install,service}.rs` (PACK). Actual CLI, native host grants, local instance/process control and activation. |

Business rules stay in their named domain/use-case modules. A new presentation or remote DTO can map onto an existing use case without a forwarding module in every crate. A shared `rusqlite::Connection` never escapes the store adapter. [Automated dependency policy](../tools/check-crate-deps.mjs) · [Rust conventions](rust-conventions.md)

## Browser source

```text
web/src/
├── main.tsx                                  SLICE → SHELL · production entry
├── app/
│   ├── App.tsx, selection.ts                 SHELL · navigation and confirmed selection
│   └── views/{QueueView,RunsView,AttentionView}.tsx    QUEUEUI
├── api/
│   ├── client.ts, events.ts                  SHELL · auth, scoped requests and global SSE
│   ├── generated/{contracts.ts,validators.mjs,validators.d.mts}   CONTRACT + integration
│   └── <resource>.ts                         feature owner · typed operations near first use
├── features/
│   ├── queue/{queue-rows.ts,BulkLaunch.tsx}   QUEUEUI · row models, selection and launch
│   ├── Archive.tsx, Notifications.tsx        QUEUEUI
│   ├── Tickets.tsx                           REVIEWUI · reusable ticket panel
│   ├── SettingsPane.tsx                      PREFUI
│   └── review/
│       ├── ReviewDrawer.tsx, DiscussionThread.tsx, Findings.tsx, Followups.tsx   REVIEWUI
│       ├── changes/{Changes.tsx,patch-document.ts}     REVIEWUI · stable code and anchors
│       ├── AgentPanel.tsx, Composer.tsx                CHATUI
│       └── transcript/{Transcript.tsx,useHistory.ts,history-window.ts}           CHATUI
├── components/                               reusable JSX and shared rich-content controls
├── hooks/useScrollPosition.ts                shared reading anchor lifecycle
├── lib/view-preferences.ts                   shared plain logic and versioned preferences
└── styles/{base,tokens}.css                   SHELL · common layout and tokens
```

The first review owns the initial Changes, transcript, composer and drawer files, with the ownership transitions shown above at its gate. The shell composes features, including sharing Tickets and passing Agent/Composer into the drawer. Feature folders do not import another feature. Shared API never imports presentation. Feature-specific behavior and styles stay with their consumer. Generated TS declarations and runtime predicates both come from the Rust schema. [Browser conventions](react-typescript-conventions.md) · [Web import checker](../web/dependency-cruiser.mjs) · [Browser plans](plans/05-browser/shell.md)

## Contracts, host assets and evidence

| Path | Writer and role |
| --- | --- |
| `contracts/v1.schema.json`, `web/tools/generate-contracts.mjs`, `fixtures/contracts/` | CONTRACT defines the schema exporter/Node generator and representative wire evidence. CONTRACT checks the development outputs in `contracts/generated/`. SLICE moves them to `web/src/api/generated/` with the first real browser caller. Integration regenerates the single active output directory whenever a registered type changes. |
| `hosts/codex/skills/reviewq/{SKILL.md,agents/openai.yaml}`, `hosts/shared/skills/reviewq-manage/SKILL.md` | SLICE → HOST. Explicit in-terminal registration and ordinary management skill. ENTRY projects managed grants for configured checkouts. |
| `hosts/claude/.claude-plugin/marketplace.json`, `hosts/claude/plugin/.claude-plugin/plugin.json`, `hosts/claude/plugin/hooks/{hooks.json,register.jsx}` | HOST owns the installable Claude integration. The host loads its own component against its API; it does not import the React browser bundle. |
| `fixtures/reviews/`, `tools/dev/seed-review`, `tools/dev/isolated-service` | FIXTURE builds reproducible provider inputs/known output; SLICE supplies the isolated production-path launcher. |
| `tools/package/assemble.mjs` | PACK's deterministic release assembly. |
| `web/tests/browser/`, `crates/reviewq/tests/` | Keep useful journey and service checks around actual entry points. Use disposable profiles and bounded inputs. Browser and release qualification reuse the same data. |
| `docs/specs/`, `docs/plans/` | User behavior, interfaces and ownership, then one implementation brief per roadmap ID. |
| `target/`, `web/dist/` | Compiled binaries and production browser assets. Release bundle includes the matching executable, web assets and host integrations. |

## Installed files

The data root holds `reviewq.sqlite3`, `releases/`, `current` and version-matched fingerprinted assets. The XDG config root holds machine-local startup options in `reviewq/runtime.json`, the private `reviewq/bridge.json` descriptor and the managed user unit. Live preferences and credentials are committed through SQLite. The state root holds profile/activation locks, recoverable install journal, verified backups and bounded diagnostics. OS user credentials protect these data. Each host keeps its own transcripts and conversations. An uninstall removes owned installation entries individually and preserves private review data. [Exact paths, defaults and ownership](specs/operations.md) · [SQLite schema](specs/persistence.md)
