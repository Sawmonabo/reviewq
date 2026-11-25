//! Publishing: `finish_review`'s closing checks, composing outside any transaction, the
//! conditional commit with at most three recomposes on a stale snapshot and the display index
//! after it; and settling, which publishes an update or records `unfinished_review` or
//! `update_failed` from the domain's `settle` decision.
