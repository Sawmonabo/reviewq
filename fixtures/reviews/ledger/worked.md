[acme/billing · !42](https://gitlab.example.com/acme/billing/-/merge_requests/42)

# Retry invoice writes without duplicates

Adds a retry loop around invoice writes so a dropped acknowledgement no longer fails the request.

The retry reuses the request's idempotency key.

**REQUEST CHANGES**

The retry still creates a new key on one path, which can write a duplicate invoice.

P0: 0 · P1: 1 · P2: 1 · P3: 1 · Blocking: 1 · Nonblocking: 2

**Scope:** Full review

**Reviewed source:** 4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a

**Reviewed target:** 9e8d7c6b9e8d7c6b9e8d7c6b9e8d7c6b9e8d7c6b

**Checked at:** 2026-11-25T14:03:07Z

## Principal code / data flow

1. `POST /invoices` reaches `InvoiceService::create`.
2. The service writes the row and returns the receipt.

## What changes across the MR

- Adds `retry_write` in `src/retry.rs`.
- Moves key creation into `src/invoices.rs`.

## Our previous threads

| Finding | Outcome and verification | Thread or evidence |
| --- | --- | --- |
| F-01 · Retry writes the same invoice twice | **Still present.** The retry path still builds a new key. | `D9` · Checked revision: `4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a` |
| F-02 · `retry_key` is rebuilt on every attempt | **Still present.** `retry_key` is still called inside the loop. | No linked discussion · Checked revision: `4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a4f3c2b1a` |
| F-03 · Two retry paths build separate keys | **Fixed.** Both paths now share one key \| verified by replay. | `D7` · Checked revision: `17a2b3c417a2b3c417a2b3c417a2b3c417a2b3c4` |

The author replied on D7 that the shared key landed in the second commit.

## Functional findings

### F-01 · Retry writes the same invoice twice

**P1 · Functional · Blocking**

**Location:** `src/invoices.rs:74`

**Issue:** The retry builds a fresh idempotency key after the first insert commits.

**Why it matters:** A lost acknowledgement creates a duplicate invoice.

**Confirmed:** Replaying the request after a dropped reply inserted two rows.

The timeline below shows the race.

#### Timeline

1. The first insert commits.
2. The reply is lost and the client retries.

Each <runner> keeps the body as a Vec<u8> until the write commits.

**Proposed comment:** Reuse the key stored with the first attempt instead of building a new one.

## Quality findings

### F-02 · `retry_key` is rebuilt on every attempt

**P2 · Reliability · Nonblocking**

**Location:** `src/retry.rs:12`, ``docs/retry`notes.md``

**Issue:** `retry_key` is called inside the loop, so each attempt gets a new key.

**Why it matters:** The server cannot tell a retry from a new request.

**Confirmed:** The unit test `retries_reuse_key` fails at the reviewed commit.

### F-04 · Retry delay is not configurable

**P3 · Quality · Nonblocking**

**Issue:** The delay between attempts is a constant.

**Why it matters:** Operators cannot slow retries during an outage.

**Evidence:** Not confirmed. The production retry settings were not available to check.

## Open questions and decisions

Should a retry give up after three attempts or after a time limit?

## Verification and limits

Ran `cargo test -p invoices` at the reviewed commit. The replay check ran against a local database only.

## Rollout plan for \*v2\* \& \`beta\` \#

Ship behind the `invoice_retry` flag.

## Publication and dispositions

- F-01 · Discussions: `D9`
- F-02 · Nonblocking · Publication: withheld · Agreed with the author to fix after the release.

No comments are posted yet.
