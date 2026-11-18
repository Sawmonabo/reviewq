# Repository rules

- Use the [implementation roadmap](docs/plans/roadmap.md) for task ownership and the [dependency decisions](docs/dependency-decisions.md) for first-use package choices.
- Read the [Rust rules](crates/AGENTS.md) for Cargo, crate, or dependency checker changes. Read the [web rules](web/AGENTS.md) for browser and frontend tooling changes. Apply both for cross-stack work.
- Implement and merge small reviewed slices in the primary checkout. The lead may add one temporary ReviewQ worktree for a second independent writer. Count paused worktrees. The independent reviewer uses the existing candidate. Before dispatch and merge, compare behavior and callers on accepted main and in both writers’ committed and uncommitted changes. Assign shared functionality once.
- After each merge, verify Git ancestry, remove merged task branches, worktrees and task-owned generated output, and check the cleanup ledger. Preserve unrelated files, shared caches and retained private acceptance evidence. Follow the [dispatch and cleanup procedure](docs/plans/roadmap.md#dispatch-and-merge).
- Trace callers from each affected entry point before changing a shared contract.
- Evaluate maintained, widely used packages and platform APIs before writing substantial custom code. Compare fit, maintenance, license, dependencies, and runtime cost. Use the candidate when it meets the requirements and reduces maintenance. Give the reason for the decision.
- Use existing checks first. Add an automated test only for a concrete regression those checks would miss. Keep the total at or below 500 first-party test cases across Rust and web. Count each parameterized case separately.
- Run Make targets from the repository root. Use `make web-install` for locked browser dependencies and `make check` for cross-stack changes.
- Keep scratch research in `.agents/tmp/`.
