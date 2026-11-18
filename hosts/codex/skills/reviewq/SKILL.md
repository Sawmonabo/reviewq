---
name: reviewq
description: Open ReviewQ or explicitly link this Codex terminal to a configured merge request.
---

Run `reviewq link TARGET` with the operator's exact target as one argument in this terminal. Let ReviewQ verify the native thread and resolve the merge request. Show the returned dashboard link and explain any reported failure. Linking leaves the conversation active in this terminal and does not start review work.

When invoked without a target, run `reviewq` to open the dashboard. Leave the conversation unlinked.
