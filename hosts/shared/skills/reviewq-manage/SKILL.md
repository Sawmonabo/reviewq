---
name: reviewq-manage
description: Find registered ReviewQ work, read an addressed transcript, or send operator-supplied input to a confirmed native lead through the configured ReviewQ tools.
---

Use ReviewQ's MCP tools from this authorized checkout. Find the review with ListReviews and confirm the exact run and lead with GetReview. Use ReadTranscript for the selected lead and follow its continuation when needed.

Use SendReviewMessage only when the operator directs input to a confirmed lead. Preserve the supplied message, create a new submission UUID once for that intent, and reuse it only to retry the same input. Report the returned receipt and distinguish queued delivery from a confirmed native entry. When the requested review or lead is ambiguous, ask the operator to select it.
