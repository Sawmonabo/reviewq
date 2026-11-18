# Rust conventions

Use the workspace's pinned Rust 1.100.0 toolchain, Edition 2024, resolver 3 and checked-in Cargo.lock. Each package inherits the workspace package settings and lints. Add a dependency in the package that first uses it. The full product ships one executable. [Workspace](../Cargo.toml) · [Rust rules](../crates/AGENTS.md) · [Dependency decisions](dependency-decisions.md)

## Workspace package names

Put each package directly in `crates/<package-name>/`. Use kebab-case for library package names, with the `reviewq-` prefix and hyphens between words. For example, `crates/reviewq-store/` contains package `reviewq-store`, which Rust imports as `reviewq_store`. Keep the `reviewq` executable in `crates/reviewq/`.

Matching names makes it easy to find a package's source from a command like `cargo check -p reviewq-store`. Cargo lists workspace members by path and defaults a new package's name to its directory name. It replaces hyphens with underscores for default library target names and uses the package name for the binary defined by `src/main.rs`. [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-members-and-exclude-fields) · [Cargo new](https://doc.rust-lang.org/cargo/commands/cargo-new.html) · [Cargo targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html)

## Put code with its owner

| Package | Owns | Example |
| --- | --- | --- |
| `reviewq-domain` | Validated identities, states, transition rules. Uses the standard library. | A run ID cannot stand in for a native session ID. |
| `reviewq-protocol` | Versioned requests, projections, errors and serialized discriminants. | A history page exposes bounded entries and a continuation. |
| `reviewq-app` | Shared use cases, transaction requirements and the small traits each operation consumes. | Send to a confirmed lead using an immutable submission identity. |
| `reviewq-store` | SQLite connection ownership, SQL, row conversion and atomic commits. | Commit the submission and its dispatch intent together. |
| `reviewq-providers` | Native host, GitLab and Jira adapters, upstream DTOs, reads and effects. | Verify the target session and observe native delivery. |
| `reviewq-transport` | HTTP, event and MCP endpoints, authentication, input limits and mapping between protocol and app types. | Browser and MCP invoke the same send use case. |
| `reviewq` | CLI, composition, installation, transfer and process lifetime. | Construct adapters and pass them into app services at startup. |

Start in the file that owns the behavior. A single operation may need only a new function. Keep field mapping at the boundary where meanings differ. A SQL row, native JSON payload and public HTTP response can each change independently. For a shared rule or identity, use one canonical domain definition. Trace the binary route, adapter, use case and all callers before changing its semantics. The [crate checker](../tools/check-crate-deps.mjs) lists every allowed workspace edge.

## Names and visibility

- Use `snake_case` files, modules, functions, fields and variables, `UpperCamelCase` types, traits and enum variants, and `SCREAMING_SNAKE_CASE` constants. Examples include `review_action.rs`, `RunId`, `HistoryReader` and `MAX_PAGE_BYTES`. For a module with children, use `history.rs` and `history/page.rs`.
- Keep definitions and fields private. Widen to `pub(super)`, `pub(crate)` or a deliberate public operation as its callers require. Re-export the stable entry from the owning library when it avoids exposing private layout. Give public behavior a short description of preconditions, outcomes and relevant failures.
- Put a type beside the operation that enforces it. A newtype distinguishes otherwise interchangeable values. A private-field struct and checked constructor enforce an invariant. An enum gives closed states and outcomes exhaustive matches. Share types in a subject module such as `run.rs`. Keep `types.rs` and `utils.rs` out of the role of workspace-wide collection points.
- Return concrete types by default. Put a small trait with the consumer when real adapters need to satisfy it. App owns the history-reading capability. Providers implement it. Use a bounded result or stream for large data. Borrow for immediate reads and pass owned values into retained work. A runtime-selected provider may use a trait object with a compatible interface.

These naming conventions follow the [Rust API guidelines](https://rust-lang.github.io/api-guidelines/naming.html). Visibility and re-exports follow [Rust module privacy](https://doc.rust-lang.org/reference/visibility-and-privacy.html).

## File order

1. Module documentation and attributes, then imports, child modules and explicit re-exports. Preserve order-sensitive macro declarations.
2. File-wide constants, followed by related types, errors and consumer traits. Include units in policy names. A value used by one operation belongs close to its first use.
3. Constructors and primary operations, then related trait implementations, private helpers and an optional `#[cfg(test)] mod tests`. Group items by the operation a reader came to understand.

For a report submission, the app accepts a validated run and typed request, asks store to commit or return the previous receipt, and calls the provider after commit. An uncertain external outcome retains its original submission ID. Callers can reconcile that receipt after restart. Keep raw provider errors inside their adapter and expose the actionable cause and captured identity without leaking private content. The transport maps the typed outcome to each client's documented status. [Entry points](plans/02-sessions/entrypoints.md)

## Checks

From the repository root, run `cargo check --locked -p <package>` while editing, `make test-package PACKAGE=<package>` for its tests, and `make crate-deps` after a manifest or package directory change. `make rust-check` runs rustfmt, Clippy on all targets with warnings denied, nextest and separate doctests. `make check` also checks package paths, crate edges and web source. `make check-low` uses one Rust compiler job and one nextest slot. `make check-cached` uses mbx for supported build steps in its separate target directory. [Make targets](../Makefile) · [nextest settings](../.config/nextest.toml)

All workspace packages forbid unsafe first-party Rust and warn on unreachable public items. The dependency checker restricts normal/build dependencies in the domain and keeps the SQLite driver in store. Public APIs can still be unused across the product. Review their real consumers, transaction boundaries, cancellation, error contracts and declaration order. Reuse existing checks and fixtures. Count each parameterized case toward the repository's 500 first-party test limit. [Repository rules](../AGENTS.md)
