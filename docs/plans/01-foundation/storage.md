# STORE · Commit a review and its recovery evidence

**Outcome.** A fresh private SQLite database admits an exact run/lead and durable message, supplies a compact scoped snapshot with its cursor, and replays its committed invalidations after reopening. This is the foundation consumed by SLICE. Later features extend this schema with their first caller. **Needs** accepted CONTRACT identity/event and scope types. [Schema and transactions](../../specs/persistence.md) · [CONTRACT](contracts.md) · [SLICE](first-review.md)

## Owned files and ports

| Files | Work and handoff |
| --- | --- |
| `crates/reviewq-store/src/connection.rs`, `crates/reviewq-store/src/schema.sql` | Create a new private store from one embedded schema, probe and reopen a matching store, and produce coherent backups. Own the read/write workers and shutdown. |
| `crates/reviewq-store/src/{events,notifications}.rs` | Durable event head, floor, ascending journal range, postcommit wake-up. `insert_notification(tx, observation) -> Result<NotificationId, StoreError>` is crate-private, transaction-local and deduplicates by occurrence; queue owns later inbox reads and desktop receipt behavior. Introduce the notification insert and its schema with the first accepted operation that records an occurrence. Later inbox and desktop reads reuse it. |
| `crates/reviewq-app/src/hosts.rs`, `crates/reviewq-store/src/hosts.rs` | Foundation declarations/implementation for exact registration, small lead lookup and conditional outbox admission/claim/outcome/read. Transfer file ownership to SLICE then HOST when these checks pass. Feature code uses the shared commit primitives directly inside this crate. |
| `crates/reviewq-store/src/queue.rs`, `crates/reviewq-app/src/queue.rs` | First scoped review projection, run summaries and minimal settings context accessor used by the real slice. Transfer the minimal queue read to SLICE then QUEUE. Retain SSR/browser/CLI independent use-case inputs. |
| Shared integration | Request `rusqlite` bundled+backup, digest, error and Tokio sync dependencies/features only at actual use. Lead applies manifests/locks/module declarations and concrete root integration. Keep SQLite and its native sys crate confined to store. [Driver decision](../../dependency-decisions.md) |

## First interfaces

```rust
// Signatures in store. Only the SqliteStore constructor and implemented app ports cross the crate.
pub async fn open(path: AbsoluteProfileDb) -> Result<SqliteStore, StoreError>;
async fn read<T, F>(&self, f: F) -> Result<T, StoreError>
where T: Send + 'static, F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, StoreError> + Send + 'static;
async fn write<T, F>(&self, f: F) -> Result<T, StoreError>
where T: Send + 'static, F: FnOnce(&mut WriteTx<'_>) -> Result<T, StoreError> + Send + 'static;
fn emit(&mut self, keys: &[ResourceKey]) -> Result<EventCursor, StoreError>; // WriteTx
```

Define concrete app ports in the modules of their first consumers. Use `RegisterLead {review, provider, profile, native_id, session_key, cwd}`, `LeadTarget`, `SubmitMessage {target, submission_id, text, purpose, report_ref}` and `DeliveryReceipt`. `register_lead(actor, input) -> Snapshot<LeadTarget>` is idempotent for the same proven binding; rebind conflicts. `admit_message(actor, input) -> Snapshot<DeliveryReceipt>`, `read_receipt(actor,target,submission_id) -> Snapshot<DeliveryReceipt>`, `claim_delivery(delivery_id, expected_state) -> Option<DeliveryClaim>`, `finish_delivery(claim, observation) -> DeliveryReceipt` each returns a typed result/error. Each admitted row has its own service-issued delivery UUID. A claim contains that UUID, the monotone fence and exact target; failure to match a fence can never confirm delivery. State labels and native evidence come from the [host contract](../02-sessions/hosts.md). Use the app event port from CONTRACT. SQL and connection handles stay inside store.

## Implement

- [ ] Under exclusive profile ownership, initialize a newly created private file in one transaction using the current schema, application ID `0x52565132` and format ID 1. Probe an existing file read-only, verify identity, structure and foreign keys before enabling writes. Leave any unsupported file and its sidecars untouched. Use SQLite's online backup API for coherent recovery copies including committed WAL frames. [Open and close](../../specs/persistence.md#open-and-close)
- [ ] Start exactly one writer and one reader with bounded 64-job channels. A `try_send` failure maps to busy or closed; a caller timeout after durable admission uses the submission lookup. Read under a short deferred transaction. Write under IMMEDIATE, rollback on any callback/commit error, then notify only after a successful commit. Worker jobs contain owned Send values. Never hold a SQLite statement, lock or guard across an await or network call.
- [ ] Implement same-run foreign keys, unique native binding and idempotency keys. Insert/return identical registration at most once. Separate an old registered run and a second repo sharing the same IID. Atomically write a new outbox item, dedup receipt and scoped event. Bind every outbox mutation to its run/session, purpose and fence. Check first admissible state and commit native outcomes separately. Reconcile dispatching/uncertain against provider proof outside SQL after restart. An older worker or watcher cannot overwrite a confirmed later native outcome.
- [ ] Resolve scoped compact reads using the read worker. Expose `resolve_selection(actor, review_id, optional_run_id) -> Snapshot<SelectedLead>` using the scoped review, same-review run and lead indexes in the wire spec. HOST adds launch storage and its scoped receipt read with the first launch caller. Fix stable sort keys, include independent logical revision and event watermark, use explicit column lists. Implement journal trim, head preservation and replay. A full SSE/client queue can shed live hints and reread SQLite. REPORT adds its bounded artifact chunk tables and indexes with the capture and serving paths.
- [ ] Expose the read-only current-schema probe to PACK. Give each feature owner the schema and typed read/commit example. One integration writer updates the schema with the first caller needing each relation or index.

## SLICE and PACK handoff

Open an absolute database path in an existing Linux/WSL local directory owned by the service user with mode 0700. Use real path components and a filesystem that supports file locks, SQLite WAL and a no-replace rename. Existing database and sidecars must be singly linked regular files owned by that user with mode 0600. `SqliteStore::open` holds one exclusive lock on the profile directory until both workers close. `SqliteStore::probe` is for a stopped profile. It verifies the schema of an ordinary checkpointed database in place. If a nonempty WAL remains, it validates its header and verifies a private copy of the DB and WAL against the immutable store ID. SQLite reads committed frames and handles an interrupted trailing frame. [SQLite WAL](https://www.sqlite.org/wal.html#read_only_databases)

Use `WorkspaceStore` to load or atomically replace verified project membership and active checkout grant epochs. A current checkout grant can read all currently configured projects. Connect the provider's verified review metadata to `QueueStore::observe_reviews` before reading the compact queue or registering its lead. Scope raw wire continuations to the signed query at transport. STORE validates the typed actor, generation, limit and run/review parent before reusing its page positions.

After `LeadStore::admit_message` commits, dispatch by the service `delivery_id`. A queued claim returns `DeliveryWork::Dispatch` with exact text and optional report reference. Recheck that claim via `authorize_dispatch` before calling the native provider. Use `DeliveryWork::Reconcile` to look for the same persisted ID after an uncertain result or interrupted dispatch. Submit exact evidence via `finish_delivery`. The delivery UUID and fence are distinct from the client retry key. Message and quote text preserve whitespace and Unicode scalar values. Confirm native host behavior through SLICE when wiring the synthetic fixture.

`SqliteStore::subscribe` yields postcommit hints. Use `EventLog::replay` for readback and missed invalidations. `SqliteStore::backup` publishes a verified online backup at an absent private path. `SqliteStore::close` waits to its deadline for accepted work and releases the profile lock after both workers exit.

Run `tools/dev/check-store` from a clean checkout with the pinned toolchain and Python 3. It generates and removes its own worked inputs and databases. It exercises the public Rust store ports, then checks backup identity, integrity and the indexed replay plan with Python SQLite. It also exercises committed WAL recovery and compares the original files before and after rejecting foreign or malformed inputs.

## Acceptance

Run `make crate-deps`, `cargo check --locked -p reviewq-store`, `make test-package PACKAGE=reviewq-store`, and integrated `make check`. Build a temporary private DB through `SqliteStore::open` and the real ports in a focused Rust integration harness or existing test. Record paths and commands of the task-owned harness for the next worker. Confirm the schema independently:

```sh
sqlite3 -readonly "$fixture_db" 'PRAGMA application_id; PRAGMA user_version; PRAGMA integrity_check; PRAGMA foreign_key_check;'
sqlite3 -readonly "$fixture_db" 'EXPLAIN QUERY PLAN SELECT cursor FROM events WHERE cursor > 10 ORDER BY cursor LIMIT 128;'
```

Expect decimal **1381388594**, version **1**, **ok**, zero FK rows and a primary-key/indexed range for the replay query. Verify permissions on DB/sidecars while open. Register two same-IID projects, race two identical admissions within one scope and one conflicting replay, and close/reopen before native dispatch. Query the application for exactly one receipt, one unchanged target and one logical mutation event; the stale attempt cannot finish a newer claim. Roll back a deliberately failed mutation and independently verify absence of both the row and event. Admit the same client UUID for two distinct authorized grants on the same lead. Verify two independently claimable delivery UUIDs and scoped receipt reads after restart. A reader that establishes a snapshot at A must see its original projection and head while a writer commits B; a fresh read sees B. Overfill the bounded queue with a blocked scratch transaction, observe busy, release the blocker and verify pending committed receipts. Inspect the digest and file mode of a foreign input before/after rejection. During SLICE rerun this recovery via normal HTTP and the bound provider. Preserve focused regression tests only where they distinguish an actual failure existing checks cannot catch. [Test policy](../../../AGENTS.md)
