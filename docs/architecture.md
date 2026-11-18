# ReviewQ architecture

ReviewQ runs one local Rust service. The browser, terminal helpers and native Codex and Claude clients address the same reviews, runs and durable receipts. SQLite owns workflow state. GitLab owns the merge request, Jira owns the ticket, and each native agent owns its conversation. Opening a review reads those sources through the service. It never starts a second conversation.

```text
Browser on loopback ─── HTTP + events ──────┐
Terminal CLI / native host callbacks ──────┼─ Rust transport → application use cases → SQLite
Ordinary agent chats ─── shared HTTP /mcp ─┘                       │
                                                     native agents · GitLab · Jira
```

## Working review

A user tracks an open MR, registers a terminal conversation as a lead or starts a new one, and can return to that exact run from Queue or Runs. The drawer obtains the MR overview, selected run, file index and native history independently. Selecting one file fetches its revision-bound patch. Opening Findings fetches the accepted receipt for that run. The Queue projection carries small summaries. A large patch, report body or transcript has its own read, limit and identity. [Wire and resource contracts](specs/data-contracts.md) · [Browser behavior](specs/browser-experience.md)

When the user sends feedback, the client freezes the displayed run, session and submission ID. The service commits a delivery intent, contacts the addressed provider, and records the native outcome. A reconnect reads the same receipt. After an ambiguous result, recovery looks for evidence of that submission before it can dispatch again. A submitted report has its own handoff receipt. The accepted version records the verified text hash, revision evidence and verdict. A local review decision, a private verdict, a published GitLab comment and a GitLab approval have separate evidence and permission. [Agent sessions](specs/agent-sessions.md) · [Review workflow](specs/review-workflow.md)

## Boundaries and ownership

| Boundary | Lives here | Consumers |
| --- | --- | --- |
| Domain | Validated identifiers and pure rules for review classification, action, report and follow-up transitions. Standard library only. | Application and adapters |
| Protocol | Owned versioned DTOs, discriminants, error envelopes and schema generation. No workspace dependencies. | Transport, CLI client and generated browser bindings |
| Application | Use cases, operation-specific input/output and ports. Callers supply resolved authority and scope. | HTTP, CLI, MCP, background tasks |
| Store | SQLite connections, embedded migrations, queries and complete transactions, including events and recoverable receipts. | Application ports implemented by the adapter |
| Providers | GitLab/Jira reads, native Codex/Claude control, bounded history access and factual observations. | Application ports implemented by the adapters |
| Transport | Axum routes, auth middleware, SSE, MCP request mapping and domain/protocol conversion. | The executable mounts the adapters |
| Binary | Build the Tokio runtime, open the profile, instantiate concrete adapters, bind local listeners, install host integrations and handle CLI lifecycle. | One installed command |
| Browser | React/TypeScript use generated DTOs and validators. One shell owns the confirmed run. Review owns the selected document, discussion mounts, native history and composer. | One origin and one production Vite build |

Workspace imports are mechanically enforced. The binary composes the store and providers through application interfaces. An HTTP handler cannot call SQLite, and a provider cannot create a separate database connection. [File ownership](project-layout.md) · [Crate dependency checker](../tools/check-crate-deps.mjs) · [Dependency decisions](dependency-decisions.md)

## Connections and backpressure

One installation has **one shared MCP service** on the daemon’s loopback HTTP listeners. Every configured checkout and both agent hosts connect to that endpoint with independently revocable authorization. Each request checks its grant and uses the existing application services. Total registered reviews, native sessions and ordinary management chats have no product quota. Cost grows with active work. Idle chats have no dedicated worker, connection pool, history watcher or native process owned by ReviewQ. Active provider profiles share native connections. Resolved checkout grants are checked against current settings. [Shared MCP service](plans/02-sessions/entrypoints.md#shared-mcp-service)

The service bounds simultaneous upstream requests, pending DB work, subscriber replay batches and retained document bytes. An operation that cannot enter a bounded queue returns a retryable busy result. After durable admission, the stored submission and its outcome remain available even if the requester disconnects. Provider I/O, native file scanning, hashing of large documents and desktop delivery happen outside SQLite transactions. Files and credentials are private to the OS account. [Persistence](specs/persistence.md) · [Provider read rules](plans/03-workspace/sources.md) · [Operations](specs/operations.md)

## State, updates and recovery

- **Workspace.** A complete scoped GitLab/Jira observation can publish a new queue generation. A failed or partial scan preserves the last good snapshot with its age and error. Registration, report history, decisions and alert receipts survive rediscovery. [Queue and Settings](specs/queue-and-settings.md)
- **Workflow.** Committing a mutation writes its invalidations atomically. A small durable SSE journal lets a browser catch up. Missing retained events trigger authoritative reads for mounted views. Ephemeral native deltas use the separately selected session and native source generation. Reopening the shell restores the confirmed selection only after verifying its identities. [Wire contract](specs/data-contracts.md)
- **Provider work.** A restart rechecks outstanding deliveries, handoffs, author-thread observations and interrupted sessions against the exact stored native binding. Recovery fences stale workers. An unavailable provider keeps its work visible with a reason. [Workflow transitions](specs/review-workflow.md)
- **Installation.** A fresh install creates a private database. A user can explicitly transfer supported state into a separate fresh candidate. Package activation checks data and installed host assets before enabling writes. [Transfer](plans/06-release/transfer.md) · [Packaging](plans/06-release/packaging.md)

## Implementation order

CONTRACT and STORE establish shared identities and transactions. FIXTURE supplies portable, independently checkable inputs. SLICE builds a real terminal-to-browser review, send and reconnect with the production adapters and a large selected document. Later tasks extend the working implementation in small merges. The lead groups tasks that touch the same behavior and assigns one writer to manifests, registries and application composition. The release gate exercises installed ordinary and registered chats, rollback, large document reading and sustained resource use. [Roadmap and work packages](plans/roadmap.md) · [First review](plans/01-foundation/first-review.md) · [Acceptance](specs/acceptance.md)
