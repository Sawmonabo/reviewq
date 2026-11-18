# Rust rules

- Follow the [Rust conventions](../docs/rust-conventions.md) for file organization, types and visibility.
- Put business rules in `reviewq-domain`, wire types in `reviewq-protocol`, and shared use cases in `reviewq-app`.
- Put SQLite access in `reviewq-store`, external integrations in `reviewq-providers`, and HTTP and MCP adapters in `reviewq-transport`. Wire adapters in the `reviewq` binary.
- Run network and provider calls outside SQLite write transactions.
- Keep items private until a caller needs access. Add dependencies with their first use. Follow the [permitted crate imports](../tools/check-crate-deps.mjs) and run `make crate-deps` after manifest changes.
- Run `cargo check --locked -p <package>` for a focused build or `make rust-check` for the workspace. Use `make check-low` to limit Rust to one build job and one test slot during cross-stack checks.
